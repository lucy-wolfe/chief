//! The acceptance criterion, asserted against the real binary and a real disk:
//! `~/.chief/log/` exists and holds a readable record of what `chief` did.
//!
//! # Why this is an integration test and not a unit one
//!
//! The defect was not that a function rendered a line badly. It was that the
//! whole PROCESS wrote nothing anywhere: `~/.chief/log/` did not exist on a
//! box that had been running companies, so a `chiefd_launch_company` that took
//! 4 minutes 34 seconds left no evidence at all and had to be chased with SSH,
//! `/proc`, `ss` and `strings` — without ever answering where the time went.
//!
//! A unit test of the sink would have been green throughout that. The only
//! test that could have failed is one that runs the shipped program and looks
//! on disk afterwards, which is what this does.
//!
//! `HOME` is a throwaway directory, so this asserts against a real filesystem
//! without touching the operator's own `~/.chief`.

// The same allowance every integration test in this workspace takes: a `tests/`
// file is its own crate, so clippy's `allow-*-in-tests` switches do not reach
// the plain helper functions beside the `#[test]` bodies.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::Path;

/// The stream `chief` appends to, under a given home.
///
/// `chief.jsonl`, not `chiefd.jsonl`: the file is named for the PROGRAM, and
/// the daemon writes its own beside it. One directory listing therefore reads
/// as a list of the programs that have run, which is the whole point of the
/// per-service name.
fn stream_path(home: &Path) -> std::path::PathBuf {
    home.join(".chief").join("log").join("chief.jsonl")
}

/// Run the real `chief` binary with a throwaway home and return its stream.
///
/// `--version` is chosen deliberately: it is the one invocation that touches
/// no beacond, no tmux, no company and no network, so what lands in the file
/// is attributable to the logging facility and to nothing else.
fn run_chief(home: &Path, args: &[&str]) -> String {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_chief"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn chief");
    assert!(status.success(), "chief {args:?} must succeed");
    std::fs::read_to_string(stream_path(home)).unwrap_or_default()
}

/// The whole point: the directory the incident report said did not exist is
/// created on demand, and the program's own start is in it.
#[test]
fn running_chief_creates_the_daemon_level_log_and_records_what_it_did() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(
        !stream_path(home.path()).exists(),
        "premise: a fresh home has no daemon-level log yet"
    );

    let body = run_chief(home.path(), &["--version"]);
    assert!(!body.is_empty(), "chief wrote no daemon-level log at all");

    let lines: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line must be valid JSON"))
        .collect();

    let start = lines
        .iter()
        .find(|line| line["event"] == "process.start")
        .expect("the process-start line must be there");

    // The fields an incident actually needs, each asserted rather than assumed.
    // `service` is the top-level key naming the program. It is deliberately
    // NOT repeated inside `detail`: one fact, one place.
    assert_eq!(start["service"], "chief");
    assert_eq!(start["level"], "info");
    assert_eq!(start["detail"]["verb"], "--version");
    assert!(start["pid"].as_u64().is_some_and(|pid| pid > 0), "a line must name its process");

    // ISO-8601 UTC with milliseconds — the shape that makes two programs'
    // streams sortable into one timeline.
    let at = start["at"].as_str().expect("every line carries a timestamp");
    assert_eq!(at.len(), 24, "not an ISO-8601 millisecond stamp: {at}");
    assert!(at.ends_with('Z') && at.contains('T') && at.contains('.'), "{at}");
}

/// A second run appends to the same stream rather than replacing it, because a
/// log that only remembers the last command cannot explain a sequence.
#[test]
fn a_second_run_appends_rather_than_replacing_the_stream() {
    let home = tempfile::tempdir().expect("tempdir");
    let first = run_chief(home.path(), &["--version"]);
    let second = run_chief(home.path(), &["--version"]);
    assert!(
        second.len() > first.len(),
        "the second run replaced the stream instead of appending to it"
    );
    assert!(second.starts_with(&first), "the first run's lines must survive the second");
    assert_eq!(
        second.lines().filter(|line| line.contains("\"process.start\"")).count(),
        2,
        "both runs must be in the one stream"
    );
}

/// The operand is never logged. `chief` arguments carry company names and,
/// through `chief create`, whatever an operator typed — the log records which
/// command ran, not what it was told.
#[test]
fn the_start_line_records_the_verb_and_never_its_operands() {
    let home = tempfile::tempdir().expect("tempdir");
    // A refused invocation still logs its start, which is the case that
    // matters: a command that failed is the one somebody investigates.
    let _ = std::process::Command::new(env!("CARGO_BIN_EXE_chief"))
        .args(["attach", "a-secret-company-name"])
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn chief");

    let body = std::fs::read_to_string(stream_path(home.path())).unwrap_or_default();
    assert!(body.contains("\"process.start\""), "a refused command still records its start");
    let start: serde_json::Value = body
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON"))
        .find(|line: &serde_json::Value| line["event"] == "process.start")
        .expect("the process-start line");
    assert_eq!(start["detail"]["verb"], "attach");
    assert!(!body.contains("a-secret-company-name"), "the start line must not carry the operand");
}

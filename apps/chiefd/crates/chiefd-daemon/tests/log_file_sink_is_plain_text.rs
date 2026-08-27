//! gh#502 — chiefd's log, when its stdout is a FILE, must be plain text.
//!
//! chiefd is a daemon: in production every one of its log lines is written
//! through `setsid` + `>> chiefd-run.log`, i.e. into a regular file that no
//! terminal ever renders. Until this test existed, the fmt subscriber wrote
//! SGR colour escapes into that file unconditionally, and the escapes landed
//! *inside* the `field=value` pairs operators grep for: the bytes on disk for
//! `docstore_mounted=true` are
//! `docstore_mounted\x1b[0m\x1b[2m=\x1b[0mtrue`, so the literal substring
//! `docstore_mounted=true` NEVER occurs and `grep 'key=value'` returns
//! nothing while the file plainly contains the fact. That silent zero refused
//! a real deploy and mis-routed several live investigations.
//!
//! The assertion is on the BYTES THAT REACH DISK, not on a formatted string,
//! because that is where the defect lives — a formatter test would have been
//! green throughout.
//!
//! The positive control matters as much as the assertion: this test also
//! requires a contiguous ASCII `key=` field to be present. Without it, a run
//! that produced an EMPTY file (wrong argv, binary that never logged) would
//! pass the "no escapes" check for the wrong reason and prove nothing. That
//! control has already earned its place once: the invocation below used to be
//! `chiefd no-such-command`, which reached a scaffold branch that logged the
//! resolved socket path. That branch is deleted — an unknown command is now
//! refused with plain usage text and no tracing event at all — and the control
//! failed loudly instead of letting this test pass on a log with no `tracing`
//! output in it whatsoever.
//!
//! Environment is deliberately hostile: `TERM`/`COLORTERM` are set to values
//! that beg for colour. Colour must be decided by "is this sink a terminal",
//! not by the environment a daemon happened to inherit.

// The workspace bans `std::fs::File` so company state cannot be written
// outside `chiefd_host`'s file seam. This test is the one place that MUST use
// the real type: the defect is defined as "what lands in a regular file when
// stdout is redirected into one", so the sink has to be a genuine `File`
// handed to the child as fd 1/2. A seam mock would be a different sink and
// would answer a different question — the same carve-out
// `parent_death_watchdog.rs` takes for a real kernel signal.
#![allow(clippy::disallowed_types)]

use std::io::Read;

/// Spawn the real binary with stdout AND stderr pointed at a real file, and
/// assert nothing but plain text arrived.
#[test]
fn a_file_sink_receives_no_ansi_escapes_and_no_nul_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("chiefd-run.log");
    let log = std::fs::File::create(&log_path).expect("create log");
    let log_err = log.try_clone().expect("clone log fd");

    // `chiefd bootstrap-store` pointed at a file that does not exist logs the
    // failure through `tracing::error!(path = %file.display(), %error, …)` —
    // exactly the `key=value` shape the trap corrupts — then exits non-zero.
    // It opens no database, binds nothing, and returns in milliseconds.
    //
    // It must be a real `tracing` event and not merely any output: the whole
    // defect lives in the fmt subscriber's ANSI decision, so a branch that
    // reaches `eprintln!` would produce a plain-text log for a reason that has
    // nothing to do with what is under test.
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .args(["bootstrap-store", "--store", "activity", "--dir"])
        .arg(dir.path())
        .arg("--file")
        .arg(dir.path().join("no-such-content.json"))
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        // Hostile on purpose: a daemon inherits these from whoever started it.
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .status()
        .expect("spawn chiefd");
    assert!(!status.success(), "a bootstrap-store with no content file must refuse to run");

    let mut bytes = Vec::new();
    std::fs::File::open(&log_path).expect("open log").read_to_end(&mut bytes).expect("read log");

    // Positive control FIRST: if this fails, every other assertion below is
    // vacuous and must not be read as evidence.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        text.contains("path="),
        "positive control failed: the log does not contain the contiguous field `path=`, so \
         this test could not have detected an escape sequence. Log was {} bytes: {text:?}",
        bytes.len()
    );

    let escapes = bytes.iter().filter(|byte| **byte == 0x1b).count();
    assert_eq!(
        escapes, 0,
        "chiefd wrote {escapes} ESC (0x1b) bytes into a non-terminal file sink; ANSI colour \
         inside `field=value` makes every operator grep for a fact return a silent zero. Log: \
         {text:?}"
    );

    let nuls = bytes.iter().filter(|byte| **byte == 0x00).count();
    assert_eq!(
        nuls, 0,
        "chiefd wrote {nuls} NUL (0x00) bytes into its log; grep switches to binary mode and \
         stops emitting lines, returning an arbitrarily stale prefix rather than an error. Log: \
         {text:?}"
    );
}

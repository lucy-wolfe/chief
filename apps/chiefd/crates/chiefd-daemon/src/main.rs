//! `chiefd` — the backend binary.
//!
//! One daemon per COMPANY DIRECTORY. `chiefd run --dir <dir>` is told one
//! thing and derives the rest: the store at `<dir>/.chief/db/chief.db`, the
//! identity keys, the logs, and the rendezvous it publishes at
//! `<dir>/.chief/run/daemon.json` so a command run in that directory can find
//! it. It owns that one SQLite database, the supervisor loop, the health
//! monitor, and all host-side effects behind `HostExecutor`.
//!
//! This binary is the wiring point for the three library crates and nothing
//! else: policy lives in `chiefd-core`, host effects in `chiefd-host`, the
//! wire surface in `chiefd-api`.
//!
//! # It is not the program an operator types
//!
//! `chiefd` — the operator client — is a DIFFERENT executable, built from
//! `crates/chief-cli`, installed beside this one, and it owns the terminal
//! surface and every operator verb. The P6 split divided them: they used to be
//! one binary, which forced the operator half to link `chiefd-core` and
//! `chiefd-host` and made the client-agnostic-backend mandate unexpressible.
//! The client `exec`s this program for the modes below, so `chiefd run` and
//! `chiefd run` are the same invocation of the same code; nothing in
//! this crate reaches back the other way, and
//! `scripts/test/backend-runtime-boundary.test.mjs` now scans this crate exactly
//! as it scans the three library crates.
//!
//! **Status.** The v1 socket surface (per-company actors, tiered readiness)
//! is not wired yet, so the default invocation still exits non-zero rather
//! than pretending to serve — a chiefd that binds and refuses everything
//! would be indistinguishable from a wedged one.
//!
//! The live modes are `run` (the per-company daemon, including the typed
//! `org_documents` docstore mount) and `docstore-only` (the standalone store
//! surface the TypeScript test harness boots).

#![forbid(unsafe_code)]

mod beacon;
mod bootstrap;
mod company_dir;
mod descriptor_budget;
mod manifest_ready;
mod rendezvous;
mod run;
mod shutdown_attribution;
mod watchdogs;

use std::process::ExitCode;

/// Every mode this binary answers, and what each one is for.
///
/// ONE table: [`usage`] renders it and [`main`] dispatches through the same
/// words. `chief-cli`'s `DAEMON_VERBS` — the list the installed `chiefd` binary
/// `exec`s into this one — is asserted equal to this table by
/// `scripts/test/model-facing-copy.test.mjs`, in both directions. A mode here
/// that the client does not forward is unreachable through the name an
/// operator has on PATH; a name the client forwards that is not here `exec`s
/// straight into the refusal below.
///
/// These are spawned by chiefd and by the test harness, never typed by a
/// person, which is why the operator client does not advertise a single one of
/// them.
const MODES: [(&str, &str); 4] = [
    ("run", "the per-company daemon loop"),
    ("bootstrap-store", "seed one chief.db document from a file"),
    ("set-actuation-config", "write a company's durable actuation mode"),
    ("clear-breaker", "resume a company whose converge breaker tripped"),
];

/// Everything `chiefd` answers, derived from [`MODES`].
///
/// Derived rather than written: a hand-kept list beside a dispatch is how nine
/// invented `chiefd` commands once shipped in model-facing copy, and this
/// binary's modes are the half a person is least likely to notice rotting
/// because almost nobody types them.
fn usage() -> String {
    let mut lines = vec![
        "chiefd — the backend. Spawned by `chief`, not typed.".to_string(),
        String::new(),
        "Usage: chiefd <mode> [options]".to_string(),
        String::new(),
    ];
    lines.extend(MODES.iter().map(|(mode, description)| format!("  {mode:<22}{description}")));
    lines.join("\n")
}

fn main() -> ExitCode {
    // The console formatter this program has always had, PLUS the daemon-level
    // file sink `chiefd_log` resolves — `<dir>/.chief/log/` for a process
    // stamped with its company directory, `$HOME/.chief/log/` for a box-wide
    // one. Both, and the gh#502
    // ANSI rule that used to be pasted here, are `chiefd_log::install`'s — it
    // is one rule and it was in three `main` functions.
    //
    // An interactive `chiefd` run in a terminal still gets colour.
    chiefd_log::install("chiefd");
    // Before anything opens a descriptor, and before the mode is even known:
    // every mode mounts the store, and the store alone costs ~54 descriptors
    // against the 256 macOS hands a process by default.
    let descriptor_budget = descriptor_budget::claim();

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    tracing::info!(
        event = "process.start",
        version = env!("CHIEF_VERSION"),
        argument_count = args.len(),
        descriptor_budget,
        "chiefd started"
    );
    if matches!(args.first().map(String::as_str), Some("--version") | Some("-V")) {
        println!("chiefd {}", env!("CHIEF_VERSION"));
        return ExitCode::SUCCESS;
    }

    // `chiefd --print-build-source-hash` — prints ONLY the raw
    // `CHIEFD_BUILD_SOURCE_HASH_HEX` value `build.rs` embedded at compile
    // time and exits. #862 (defect 1): a binary-currency precondition check
    // spawns exactly this — a cheap, synchronous, no-port, no-HTTP-round-trip
    // call — rather than trusting
    // the binary FILE's own on-disk mtime, which `git
    // checkout`/`tar`/`rsync`/CI-artifact-restore can all reset independent
    // of actual content. A dedicated flag, not folded into `--version`:
    // the two answer different questions (what release is this vs. what
    // exact source tree produced THIS ARTIFACT) and a precondition check
    // parsing free-form version text to extract a fingerprint would be
    // exactly the "structural fact, not a string scrape" mistake this
    // program spent today correcting elsewhere. #979: renamed from
    // `--print-build-fingerprint` when the embedded value changed from a
    // wall-clock timestamp to a content hash — see build.rs for why a
    // timestamp cannot survive a two-job CI artifact boundary.
    if matches!(args.first().map(String::as_str), Some("--print-build-source-hash")) {
        println!("{}", env!("CHIEFD_BUILD_SOURCE_HASH_HEX"));
        return ExitCode::SUCCESS;
    }

    let mode = args.first().cloned().unwrap_or_default();
    let rest = args.into_iter().skip(1);
    match mode.as_str() {
        // Serve JUST the typed org_documents docstore surface, nothing else
        // (no duty scheduler, no runtime). A distinct, greppable mode for the
        // same reason the others are: an operator — or the TypeScript test
        // harness that spawns it per test process — reads the command line and
        // knows this process only serves the document store.
        // The real per-company daemon loop (one-daemon migration). Distinct
        // mode for the same greppability reason: an operator reading the
        // command line sees a daemon that actually drives duties.
        "run" => run::run(rest),
        // Raw seed of one chief.db document from a file (see bootstrap.rs's
        // run_bootstrap_store doc for why: activity/supervision/etc need their
        // REAL content copied in, not a blank ledger).
        "bootstrap-store" => bootstrap::run_bootstrap_store(rest),
        // The operator control-plane write for a live company's durable
        // actuation mode / sweep flag / budget override.
        "set-actuation-config" => bootstrap::run_set_actuation_config(rest),
        // The explicit operator acknowledgement that resumes a company whose
        // converge circuit breaker tripped.
        "clear-breaker" => bootstrap::run_clear_breaker(rest),
        // Every mode this binary has is claimed above. What is left is
        // something this program does not do, and THIS binary refuses it — with
        // its own usage, on stderr, at exit 1.
        //
        // `eprintln!` and not `tracing::error!`: this is a usage answer to a
        // person or a script, not a daemon event, and the log formatter's
        // key=value colouring has no business in it.
        unknown => {
            eprintln!("chiefd: unknown mode '{unknown}'\n\n{}", usage());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{usage, MODES};

    /// The table and the dispatch are the same words.
    ///
    /// Asserted against this file's own source, in the idiom the operator
    /// client's `main.rs` already uses: a mode listed but never matched would
    /// print in the usage and then fall through to the refusal, which is the
    /// "documented, implemented, unreachable" shape that cost this program a
    /// whole front door once already.
    #[test]
    fn every_listed_mode_is_a_dispatch_arm_and_appears_in_the_usage() {
        let source = include_str!("main.rs");
        let text = usage();
        for (mode, description) in MODES {
            assert!(
                source.contains(&format!("\"{mode}\" => ")),
                "'{mode}' is listed but nothing dispatches it"
            );
            assert!(text.contains(mode), "'{mode}' is dispatched but the usage omits it");
            assert!(text.contains(description), "'{mode}' has no description in the usage");
        }
    }

    /// The operator verbs belong to the OTHER binary, and this one must not
    /// pretend to have them.
    ///
    /// Before P6 this binary answered `new`, `ls`, `attach`, `stop` and
    /// `reset` itself. They moved to `chief-cli` whole; a mode of the same
    /// name reappearing here would mean two programs answer one word, which is
    /// exactly the ambiguity the split exists to remove.
    #[test]
    fn no_operator_verb_is_answered_by_the_daemon() {
        let source = include_str!("main.rs");
        for verb in ["new", "ls", "attach", "stop", "reset", "create", "host"] {
            assert!(!MODES.iter().any(|(mode, _)| *mode == verb), "'{verb}' is not a daemon mode");
            assert!(
                !source.contains(&format!("\"{verb}\" => ")),
                "'{verb}' is the operator client's verb; this binary must not dispatch it"
            );
        }
    }
}

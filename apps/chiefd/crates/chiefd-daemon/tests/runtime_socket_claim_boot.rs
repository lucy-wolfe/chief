//! **A company created before per-company sockets must still start.**
//!
//! `cb63690a0` moved `chief`'s last socket tier off the shared string
//! `"default"` — the socket a bare `tmux` uses, and therefore one server for
//! every company on the box — and onto the company's own key. Every company
//! created BEFORE it holds a live runtime-ownership claim naming `default`,
//! and the claim is the company's own record of where it runs. The daemon
//! refuses to actuate on a server the claim does not name, because doing so
//! converges a second, shadow fleet.
//!
//! What the operator saw on their real company was none of that:
//!
//! ```text
//! $ chief
//! chiefd for /root/workspace (pid 21233) did not become healthy within 15s
//! ```
//!
//! This file drives the boot the way the operator's `chief` drives it — the
//! environment variable and the argv flag, nothing else — so it says the same
//! thing on either side of the fix rather than following the code.
//!
//! It is a `tests/` integration test because it needs `CARGO_BIN_EXE_chiefd`.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use host_primitives::rendezvous::company_key;

const DEADLINE: Duration = Duration::from_secs(30);

/// The store a company directory holds, stated literally: this file is an
/// outside witness and must not derive the answer from the daemon's helper.
fn store_db_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join("db").join("chief.db")
}

/// Spawn `chiefd run --once` and wait for it to exit.
///
/// `--once` runs the startup self-audit and one duty pass, then exits — which
/// is past the socket resolution this file is about, and short of beacond
/// admission, so it needs no beacond.
///
/// `demanded` is the `--runtime-socket` flag, which only a human (or a test
/// harness pinning a throwaway server) passes; `preferred` is
/// `ORG_LAUNCHER_RUNTIME_SOCKET`, which is what `chief` sets at every spawn.
fn run_once(
    dir: &Path,
    logs: &Path,
    log_name: &str,
    demanded: Option<&str>,
    preferred: Option<&str>,
) -> (std::process::ExitStatus, String) {
    let log_path = logs.join(log_name);
    let log = std::fs::File::create(&log_path).expect("log file");
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"));
    command
        .arg("run")
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir)
        .arg("--launcher-root")
        .arg(logs)
        .arg("--once")
        .env_remove("ORG_LAUNCHER_RUNTIME_SOCKET")
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log));
    if let Some(demanded) = demanded {
        command.arg("--runtime-socket").arg(demanded);
    }
    if let Some(preferred) = preferred {
        command.env("ORG_LAUNCHER_RUNTIME_SOCKET", preferred);
    }
    let mut child = command.spawn().expect("chiefd run --once spawns");

    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "chiefd run --once for {} did not exit within {DEADLINE:?}. Log:\n{}",
            dir.display(),
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, std::fs::read_to_string(&log_path).unwrap_or_default())
}

/// Write the runtime-ownership claim a pre-`cb63690a0` company carries: active,
/// naming the shared `default` server.
///
/// Raw SQL against the daemon's own schema, on purpose. The claim is the fact
/// under test, and seeding it through the very code path that decides what to
/// do with it would let both sides move together.
fn seed_a_live_claim(dir: &Path, socket: &str) {
    let key = company_key(dir);
    let conn = rusqlite::Connection::open(store_db_path(dir)).expect("open the company store");
    conn.execute(
        "INSERT INTO runtime_owner(slug, status, socket, claimed_at, validated_at, released_at) \
         VALUES(?1,'active',?2,'2026-08-01T00:00:00.000Z','2026-08-01T00:00:00.000Z',NULL) \
         ON CONFLICT(slug) DO UPDATE SET status='active', socket=?2, released_at=NULL",
        rusqlite::params![key, socket],
    )
    .expect("seed the runtime-ownership claim");
}

/// A company directory with the daemon's own schema already in it.
fn a_company_that_has_booted_before(parent: &Path) -> PathBuf {
    let dir = parent.join("a-company-that-has-booted-before");
    std::fs::create_dir_all(&dir).expect("company directory");
    let (status, log) = run_once(&dir, parent, "first.log", None, None);
    assert!(status.success(), "fixture: the first boot must succeed. Log:\n{log}");
    assert!(store_db_path(&dir).is_file(), "fixture: the first boot writes the store");
    dir
}

/// THE UPGRADE, end to end through the daemon's real argv and environment.
///
/// `chief` cannot read a company's claim before a daemon serves it, so the
/// socket it names at spawn is a GUESS. A guess must lose to the company's own
/// record of where it runs — otherwise every company that existed before
/// `cb63690a0` is un-startable, which is exactly what shipped.
#[test]
fn a_company_claiming_the_shared_socket_still_boots_when_the_client_guesses_its_key() {
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = a_company_that_has_booted_before(parent.path());
    seed_a_live_claim(&dir, "default");

    let key = company_key(&dir);
    let (status, log) = run_once(&dir, parent.path(), "upgrade.log", None, Some(&key));

    assert!(
        status.success(),
        "a company whose claim names 'default' must still start when the client's own \
         preference is its key. This is the operator's 'did not become healthy within 15s'. \
         Log:\n{log}"
    );
    assert!(
        log.contains("adopted-from-runtime-owner"),
        "and it must run where the claim says, not where the guess said. Log:\n{log}"
    );
    assert!(
        !log.contains("refusing to start"),
        "nothing about this pair is an operator error. Log:\n{log}"
    );
}

/// The invariant, unweakened: a socket an operator DEMANDED that contradicts a
/// live claim is still refused, because actuating there would converge a
/// second fleet onto a server the company does not run on.
///
/// And the refusal is actionable now. It used to end "or release the claim
/// first" without saying how, so the only stated recovery an operator could
/// act on was a flag they had to find in a log file.
#[test]
fn a_demanded_socket_contradicting_the_claim_is_still_refused_and_names_the_command() {
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = a_company_that_has_booted_before(parent.path());
    seed_a_live_claim(&dir, "default");

    let key = company_key(&dir);
    let (status, log) = run_once(&dir, parent.path(), "demanded.log", Some(&key), None);

    assert!(!status.success(), "a contradicted demand must not actuate. Log:\n{log}");
    assert!(log.contains("shadow fleet"), "the refusal says what it prevents. Log:\n{log}");
    assert!(log.contains("chief stop"), "and names the command that ends the claim. Log:\n{log}");
}

/// A released claim is not a claim: the company comes back on whatever the
/// client prefers, which after `chief stop` is its own key.
#[test]
fn a_released_claim_leaves_the_clients_preference_standing() {
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = a_company_that_has_booted_before(parent.path());
    seed_a_live_claim(&dir, "default");
    let conn = rusqlite::Connection::open(store_db_path(&dir)).expect("open the company store");
    conn.execute(
        "UPDATE runtime_owner SET status='released', released_at='2026-08-18T00:00:00.000Z'",
        [],
    )
    .expect("release the claim");
    drop(conn);

    let key = company_key(&dir);
    let (status, log) = run_once(&dir, parent.path(), "released.log", None, Some(&key));

    assert!(status.success(), "a released company starts. Log:\n{log}");
    assert!(
        log.contains("client-preference"),
        "and nobody claiming means the client's own socket stands. Log:\n{log}"
    );
}

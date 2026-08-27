//! D19 — a genuine crash mid-transaction leaves the store exactly as it
//! was. Each test drives TWO real connections against the SAME file: one
//! that opens a transaction and is dropped WITHOUT commit (the actual
//! mechanism a `SIGKILL` invokes — SQLite rolls an unfinished transaction
//! back when the connection that held it closes), and a second, fresh
//! [`beacond::store::Registry`] that re-opens the file afterward to observe
//! whatever survived. This is not an injected `Err`: it is the real crash
//! path, exercised for real.
//!
//! These live here rather than in `src/store.rs` so their own raw
//! `BEGIN IMMEDIATE` calls (needed to simulate the crashing connection)
//! don't inflate that file's `BEGIN IMMEDIATE` count past the five real
//! ones the D19 acceptance grep counts.

use beacond::store::Registry;
use beacond::wire::Location;
use rusqlite::Connection;

/// The company directory under test, and the key its caller minted for it.
const DIR: &str = "/work/northstar";
const KEY: &str = "0123456789ab";

fn location(dir: &str, pid: i64) -> Location {
    Location {
        dir: dir.to_string(),
        url: "http://127.0.0.1:8794".to_string(),
        port: 8794,
        pid,
        hostname: "box".to_string(),
    }
}

#[test]
fn a_crash_mid_create_leaves_no_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("beacond.sqlite");
    let registry = Registry::open(&path).expect("open");
    drop(registry);

    // Simulate a crash: open our OWN connection, start the write, but
    // never commit — then drop it, exactly as a killed process would.
    {
        #[allow(clippy::disallowed_methods)] // crash-simulation seam, see module doc
        let crashing = Connection::open(&path).expect("open for crash simulation");
        crashing.execute("BEGIN IMMEDIATE", []).expect("begin");
        crashing
            .execute(
                "INSERT INTO companies(dir, key, slug, registered_at) VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![DIR, KEY, "northstar", "t0"],
            )
            .expect("insert");
        // No COMMIT. `crashing` drops here, uncommitted.
    }

    let reopened = Registry::open(&path).expect("reopen after crash");
    assert_eq!(
        reopened.list().expect("list"),
        Vec::new(),
        "the uncommitted insert must not survive"
    );
}

#[test]
fn a_crash_mid_register_leaves_the_previous_location_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("beacond.sqlite");
    let registry = Registry::open(&path).expect("open");
    registry.create(DIR, KEY, "northstar", "t0").expect("create");
    registry.register(&location(DIR, 111)).expect("initial register");
    let before = registry.lookup(DIR).expect("lookup").expect("row exists");
    drop(registry);

    // Simulate a crash mid-register: read the previous row (the same
    // admission read `Registry::register` performs), start the location
    // UPDATE, then never commit.
    {
        #[allow(clippy::disallowed_methods)] // crash-simulation seam, see module doc
        let crashing = Connection::open(&path).expect("open for crash simulation");
        crashing.execute("BEGIN IMMEDIATE", []).expect("begin");
        let _existing_pid: Option<i64> = crashing
            .query_row("SELECT pid FROM companies WHERE dir = ?1", [DIR], |row| row.get(0))
            .expect("read previous row");
        crashing
            .execute(
                "UPDATE companies SET url = ?1, port = ?2, pid = ?3, hostname = ?4 WHERE dir = ?5",
                rusqlite::params!["http://127.0.0.1:9999", 9999_i64, 222_i64, "other-box", DIR],
            )
            .expect("update");
        // No COMMIT.
    }

    let reopened = Registry::open(&path).expect("reopen after crash");
    let after = reopened.lookup(DIR).expect("lookup").expect("row exists");
    assert_eq!(after, before, "a crashed register must leave the previous location intact");
    // A failed registration must never make a live company unreachable: the
    // OLD pid is still the recorded, lookup-able owner.
    assert_eq!(after.pid, Some(111));
}

#[test]
fn a_crash_mid_deregister_leaves_the_location_intact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("beacond.sqlite");
    let registry = Registry::open(&path).expect("open");
    registry.create(DIR, KEY, "northstar", "t0").expect("create");
    registry.register(&location(DIR, 111)).expect("register");
    let before = registry.lookup(DIR).expect("lookup").expect("row exists");
    drop(registry);

    {
        #[allow(clippy::disallowed_methods)] // crash-simulation seam, see module doc
        let crashing = Connection::open(&path).expect("open for crash simulation");
        crashing.execute("BEGIN IMMEDIATE", []).expect("begin");
        let _existing_pid: Option<i64> = crashing
            .query_row("SELECT pid FROM companies WHERE dir = ?1", [DIR], |row| row.get(0))
            .expect("read previous row (the pid fence)");
        crashing
            .execute(
                "UPDATE companies SET url = NULL, port = NULL, pid = NULL, hostname = NULL, \
                 last_seen_at = NULL WHERE dir = ?1",
                [DIR],
            )
            .expect("clear");
        // No COMMIT.
    }

    let reopened = Registry::open(&path).expect("reopen after crash");
    let after = reopened.lookup(DIR).expect("lookup").expect("row exists");
    assert_eq!(after, before, "a crashed deregister must leave the location intact");
}

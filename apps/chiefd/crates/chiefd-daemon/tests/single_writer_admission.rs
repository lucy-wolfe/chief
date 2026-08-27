//! **Single-writer admission (E10-S3, #764): a second `chiefd run` against
//! one company refuses — loudly and crash-honestly — arbitrated by beacond,
//! not a local owner-marker file.**
//!
//! The owner marker (`chiefd_host::run_admission`, `.{slug}.chiefd-owner
//! .json`) is DELETED by this story (ruling D19/D20/D23-F16): a lock file
//! and unsanctioned disk state. Its job moves to beacond's `POST
//! /v1/register` — one conditional UPDATE of the company's location
//! columns, inside one transaction. The SAME three properties this suite
//! always pinned survive the rewrite, against real spawned daemons and a
//! real spawned beacond:
//!
//! (a) LOUD — while a daemon owns company A, a second `chiefd run` for A
//!     refuses (409 from beacond) and its log names the live owner (pid,
//!     hostname, `lastSeenAt`). A silently declining second daemon is a
//!     company that mysteriously stops converging.
//! (b) CRASH-HONEST — after the owner is `kill -9`'d (no deregister, so
//!     the location survives with a dead pid), a new daemon for A starts
//!     and is ADMITTED: beacond's row is taken over inside one
//!     transaction, no TTL, no cleanup step, no manual intervention.
//! (c) SCOPED — a daemon for the company in directory B coexists with the
//!     daemon for the company in directory A: each `register` call names its
//!     own DIRECTORY, so admission is per-company, never per-machine. Two
//!     directories may even hold companies of the same NAME, which is the
//!     case the slug-keyed registry could not represent at all.
//!
//! Plus the property the marker file COULD NOT have expressed (there was
//! nothing to ask): (d) a daemon cannot invent a company by binding —
//! `chiefd run --dir <never-created>` refuses with beacond's
//! `unknown-company` answer and beacond's row set is unchanged — while a
//! directory that HOLDS a company database restores its lost registry row
//! at boot (the operator's 2026-08-26 repeal; the proof is the database,
//! never the binding) and the restoration is loud exactly once.
//!
//! It is a `tests/` integration test because it needs `CARGO_BIN_EXE_chiefd`
//! /`CARGO_BIN_EXE_beacond` and because only real processes can be killed
//! for real.

// Real second processes, real kernel signals, real wall-clock deadlines: the
// separate-process exception to the injected-clock rule (`chiefd_core::clock`).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use http_body_util::BodyExt as _;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;

/// How long a daemon gets to reach admission / exit on refusal, and how
/// long beacond gets to become reachable.
const DEADLINE: Duration = Duration::from_secs(30);

type Client = HyperClient<HttpConnector, http_body_util::Full<hyper::body::Bytes>>;

fn http_client() -> Client {
    HyperClient::builder(TokioExecutor::new()).build_http()
}

async fn post(client: &Client, url: &str, body: serde_json::Value) -> (u16, String) {
    let request = hyper::Request::builder()
        .method("POST")
        .uri(url)
        .header("connection", "close")
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(hyper::body::Bytes::from(body.to_string())))
        .expect("build request");
    let response = client.request(request).await.expect("send request");
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.expect("collect body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf8 body"))
}

/// `None` on any transport failure (connection refused, most commonly a
/// server that has not started listening yet) rather than panicking — the
/// ONLY caller is a readiness poll, which must tolerate "not up yet" as a
/// normal, expected, retried outcome rather than a test failure.
async fn try_get(client: &Client, url: &str) -> Option<(u16, String)> {
    let request = hyper::Request::builder()
        .method("GET")
        .uri(url)
        .header("connection", "close")
        .body(http_body_util::Full::new(hyper::body::Bytes::new()))
        .expect("build request");
    let response = client.request(request).await.ok()?;
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.ok()?.to_bytes();
    Some((status, String::from_utf8(bytes.to_vec()).ok()?))
}

async fn get(client: &Client, url: &str) -> (u16, String) {
    let request = hyper::Request::builder()
        .method("GET")
        .uri(url)
        .header("connection", "close")
        .body(http_body_util::Full::new(hyper::body::Bytes::new()))
        .expect("build request");
    let response = client.request(request).await.expect("send request");
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.expect("collect body").to_bytes();
    (status, String::from_utf8(bytes.to_vec()).expect("utf8 body"))
}

struct Child {
    child: std::process::Child,
    log_path: PathBuf,
}

impl Child {
    fn read_log(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log_path).unwrap_or_default())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // A test that failed mid-way must not leak a daemon onto the box.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A spawned real `beacond`, killed on drop.
struct Beacond {
    child: std::process::Child,
    url: String,
    log_path: PathBuf,
}

impl Drop for Beacond {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `CARGO_BIN_EXE_beacond` is not populated for this cross-package
/// dependency in this workspace's configuration (verified: neither
/// `cargo build --tests` nor `cargo test --no-run` sets it, even with
/// `beacond` as an explicit dev-dependency here purely to force Cargo to
/// build its binary as part of this compile). Every OTHER binary-spawning
/// test in this workspace spawns its OWN package's binary
/// (`CARGO_BIN_EXE_chiefd` from within `chiefd`'s own tests) -- this is the
/// first test that needs a SIBLING package's binary, so the well-established
/// fallback applies: both binaries land in the same Cargo target directory
/// (`target/<profile>/`), so `beacond` is `chiefd`'s own binary path with
/// the filename swapped. The `beacond` dev-dependency above still does its
/// job -- it guarantees the binary is actually BUILT before this test runs.
fn beacond_bin_path() -> PathBuf {
    let chiefd_bin = PathBuf::from(env!("CARGO_BIN_EXE_chiefd"));
    chiefd_bin.parent().expect("CARGO_BIN_EXE_chiefd has a parent directory").join("beacond")
}

async fn spawn_beacond(dir: &Path, client: &Client) -> Beacond {
    let db_path = dir.join("beacond.sqlite");
    let log_path = dir.join("beacond.log");
    let log = std::fs::File::create(&log_path).expect("beacond log file");
    let child = std::process::Command::new(beacond_bin_path())
        // The child owns port selection. A released `free_port()` can be
        // claimed by the sibling test before this process binds, which made
        // both tests accept one beacond and share its registry.
        .env("BEACOND_BIND", "127.0.0.1:0")
        .env("BEACOND_DB_PATH", &db_path)
        .stdout(std::process::Stdio::from(log.try_clone().expect("duplicate beacond log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("beacond spawns");
    let mut beacond = Beacond { child, url: String::new(), log_path };
    let deadline = Instant::now() + DEADLINE;
    loop {
        let log = strip_ansi(&std::fs::read_to_string(&beacond.log_path).unwrap_or_default());
        if let Some(url) = bound_url_from_log(&log) {
            if let Some((200, _)) = try_get(client, &format!("{url}/v1/health")).await {
                assert_beacond_running(&mut beacond, &log);
                beacond.url = url;
                return beacond;
            }
        }
        assert_beacond_running(&mut beacond, &log);
        assert!(
            Instant::now() < deadline,
            "beacond never published a healthy listener. Log:\n{log}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_beacond_running(beacond: &mut Beacond, log: &str) {
    match beacond.child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!("beacond exited before readiness ({status}). Log:\n{log}"),
        Err(error) => panic!("could not inspect beacond before readiness ({error}). Log:\n{log}"),
    }
}

fn bound_url_from_log(log: &str) -> Option<String> {
    log.lines().rev().filter(|line| line.contains("beacond listening")).find_map(|line| {
        let raw = line
            .split_ascii_whitespace()
            .find_map(|field| field.strip_prefix("bind="))?
            .trim_matches('"');
        let address = raw.parse::<std::net::SocketAddr>().ok()?;
        (address.ip().is_loopback() && address.port() != 0).then(|| format!("http://{address}"))
    })
}

/// The company key. beacond records it verbatim and checks only its SHAPE, so
/// a test that registers a company must mint it the way the daemon does —
/// through the one shared definition, never a private copy of the hash.
use host_primitives::rendezvous::company_key;

/// A company directory under `parent`, created and ready to be registered.
fn company_directory(parent: &Path, name: &str) -> std::path::PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("company directory");
    dir
}

async fn create_company(client: &Client, beacond_url: &str, dir: &Path, slug: &str) {
    let (status, body) = post(
        client,
        &format!("{beacond_url}/v1/company/create"),
        serde_json::json!({
            "dir": dir.display().to_string(),
            "key": company_key(dir),
            "slug": slug,
        }),
    )
    .await;
    assert_eq!(status, 200, "company/create for {} failed: {body}", dir.display());
}

async fn lookup(client: &Client, beacond_url: &str, dir: &Path) -> serde_json::Value {
    let (status, body) =
        get(client, &format!("{beacond_url}/v1/lookup?dir={}", dir.display())).await;
    assert_eq!(status, 200, "lookup for {} failed: {body}", dir.display());
    serde_json::from_str(&body).expect("lookup body is JSON")
}

/// Spawn `chiefd run --dir <company>`, with its log written OUTSIDE the
/// company directory: this suite inspects that directory for storage a refused
/// daemon must not have created, so the harness must leave nothing there.
fn spawn_run(company: &Path, logs: &Path, log_name: &str, port: u16, beacond_url: &str) -> Child {
    let log_path = logs.join(log_name);
    let log = std::fs::File::create(&log_path).expect("log file");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(company)
        .arg("--launcher-root")
        .arg(logs)
        // A socket name no runtime server answers on: this test is about
        // admission, and it must never touch an operator's panes.
        .arg("--runtime-socket")
        .arg(format!("admission-{}-throwaway", company_key(company)))
        .env("CHIEFD_STORE_BIND", format!("127.0.0.1:{port}"))
        // No walk: this suite drives EXACT ports it already knows are free
        // (`free_port()`), so a walk would only obscure which candidate
        // actually got claimed. Single-attempt is `bind_walking`'s own
        // documented behaviour for `walk == 1`.
        .env("CHIEFD_STORE_PORT_WALK", "1")
        .env("BEACOND_URL", beacond_url)
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run spawns");
    Child { child, log_path }
}

/// This pid is named as the current owner in beacond's row for `dir`,
/// within the deadline. Polling beacond (the new source of truth) rather
/// than a marker file — this IS the property under test.
async fn wait_for_admission(client: &Client, beacond_url: &str, dir: &Path, pid: u32) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let company = lookup(client, beacond_url, dir).await;
        if company["found"] == serde_json::json!(true) {
            if let Some(recorded_pid) = company["company"]["pid"].as_i64() {
                if recorded_pid == i64::from(pid) {
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon pid {pid} never appeared as {}'s owner in beacond",
            dir.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "the refused second daemon never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn readiness_uses_only_the_spawned_beaconds_published_loopback_endpoint() {
    let log = "INFO beacond: beacond listening bind=127.0.0.1:49271 db=/tmp/one.sqlite";
    assert_eq!(bound_url_from_log(log), Some("http://127.0.0.1:49271".to_string()));
    assert_eq!(
        bound_url_from_log("INFO beacond: beacond listening bind=0.0.0.0:49271"),
        None,
        "an unrelated non-loopback listener cannot become this test's subject"
    );
    assert_eq!(
        bound_url_from_log("INFO beacond: beacond listening bind=127.0.0.1:0"),
        None,
        "the kernel-selected endpoint must carry a usable port"
    );
}

fn send(pid: u32, signal: nix::sys::signal::Signal) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).expect("pid fits")),
        signal,
    )
    .expect("signal is delivered");
}

/// (a) LOUD, (c) SCOPED, (b) CRASH-HONEST — pinned together the same way the
/// marker-based predecessor pinned them, now against real beacond.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn second_run_refuses_loudly_and_stale_registration_is_reclaimed_and_companies_coexist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = http_client();
    let beacond = spawn_beacond(dir.path(), &client).await;
    // TWO DIRECTORIES, ONE NAME. The case the slug-keyed registry could not
    // represent: keyed by slug these were one row, so the second company's
    // daemon read the first as a live incumbent and refused.
    let northstar = company_directory(dir.path(), "northstar");
    let elsewhere = company_directory(dir.path(), "another-place");
    create_company(&client, &beacond.url, &northstar, "northstar").await;
    create_company(&client, &beacond.url, &elsewhere, "northstar").await;

    // (a) LOUD: a second `chiefd run` against the same company — which now
    // means the same DIRECTORY — refuses and names the live owner.
    let mut first = spawn_run(&northstar, dir.path(), "first.log", free_port(), &beacond.url);
    let first_pid = first.child.id();
    wait_for_admission(&client, &beacond.url, &northstar, first_pid).await;

    let mut second = spawn_run(&northstar, dir.path(), "second.log", free_port(), &beacond.url);
    let status = wait_for_exit(&mut second.child);
    assert!(
        !status.success(),
        "a second `chiefd run` against one company must refuse, got {status:?}"
    );
    let second_log = second.read_log();
    assert!(
        second_log.contains("refusing to start a second daemon against one company"),
        "the refusal must be loud and greppable. Log:\n{second_log}"
    );
    assert!(
        second_log.contains(&format!("incumbent_pid={first_pid}"))
            || second_log.contains(&format!("incumbent_pid: {first_pid}")),
        "the refusal must name the live owner's pid ({first_pid}). Log:\n{second_log}"
    );
    // The "a refused contender opens no storage" half of #764 is witnessed by
    // `a_daemon_cannot_invent_a_company_by_binding` below, against a directory
    // no daemon ever owned. It CANNOT be witnessed here any more: the two
    // contenders are necessarily the same directory now, so the winner's own
    // database, WAL and schema are already there and prove nothing about the
    // loser. Asserting their absence here would be a test that could only fail
    // for the wrong reason.

    // beacond's own row still names the SURVIVOR's pid, not a corrupted or
    // cleared value — the atomic UPDATE never let the loser's write apply.
    let after_race = lookup(&client, &beacond.url, &northstar).await;
    assert_eq!(after_race["company"]["pid"], serde_json::json!(i64::from(first_pid)));

    // (c) SCOPED: a company in ANOTHER DIRECTORY — carrying the very same
    // name — admits while the first daemon still owns this one. Neither
    // refuses the other.
    let mut other = spawn_run(&elsewhere, dir.path(), "other.log", free_port(), &beacond.url);
    let other_pid = other.child.id();
    wait_for_admission(&client, &beacond.url, &elsewhere, other_pid).await;
    assert!(
        first.child.try_wait().expect("try_wait").is_none(),
        "the northstar daemon must be unaffected by a same-named company in another directory"
    );
    send(other_pid, nix::sys::signal::Signal::SIGTERM);
    let _ = wait_for_exit(&mut other.child); // graceful stop deregisters

    // (b) CRASH-HONEST: SIGKILL the owner — no deregister, so the location
    // survives with a dead pid — and a new daemon for the same company
    // starts immediately and is ADMITTED, no cleanup step involved.
    send(first_pid, nix::sys::signal::Signal::SIGKILL);
    let killed = wait_for_exit(&mut first.child);
    assert!(!killed.success(), "kill -9 is not a clean exit: {killed:?}");
    let stale = lookup(&client, &beacond.url, &northstar).await;
    assert_eq!(
        stale["company"]["pid"],
        serde_json::json!(i64::from(first_pid)),
        "a killed daemon leaves its stale location behind (never cleaned up here)"
    );

    let mut restarted =
        spawn_run(&northstar, dir.path(), "restarted.log", free_port(), &beacond.url);
    let restarted_pid = restarted.child.id();
    wait_for_admission(&client, &beacond.url, &northstar, restarted_pid).await;
    let reclaimed = lookup(&client, &beacond.url, &northstar).await;
    assert_eq!(
        reclaimed["company"]["pid"],
        serde_json::json!(i64::from(restarted_pid)),
        "the restarted daemon reclaimed the location from the dead owner"
    );
    send(restarted_pid, nix::sys::signal::Signal::SIGTERM);
    let _ = wait_for_exit(&mut restarted.child);
}

/// (d) A daemon cannot invent a company by binding: no row, no admission,
/// and beacond gains nothing from the attempt. The marker-based predecessor
/// could not express this property at all — a marker was created by
/// whoever asked first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn a_daemon_cannot_invent_a_company_by_binding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = http_client();
    let beacond = spawn_beacond(dir.path(), &client).await;
    // The directory exists — a daemon must be able to resolve it — but no
    // company was ever registered for it.
    let ghost_dir = company_directory(dir.path(), "never-created");

    let mut ghost = spawn_run(&ghost_dir, dir.path(), "ghost.log", free_port(), &beacond.url);
    let status = wait_for_exit(&mut ghost.child);
    assert!(!status.success(), "a daemon for an unknown company must refuse, got {status:?}");
    let log = ghost.read_log();
    assert!(
        log.contains("beacond has no company row for this directory"),
        "the refusal must name the unknown-company reason. Log:\n{log}"
    );

    let (status, body) = get(&client, &format!("{}/v1/list", beacond.url)).await;
    assert_eq!(status, 200);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("list body is JSON");
    assert_eq!(
        parsed["companies"],
        serde_json::json!([]),
        "beacond must gain no row from a refused registration attempt"
    );
    // THE DISK WITNESS for #764's "no storage before admission": the refusal
    // arrives before `CompanyDb::open`, so this directory must hold no
    // `.chief/` at all — no store, no WAL, no schema, and no rendezvous.
    assert!(
        !ghost_dir.join(".chief").exists(),
        "an unknown-company refusal must create nothing under {}",
        ghost_dir.join(".chief").display()
    );
}

/// The operator's 2026-08-26 repeal, red-first: a directory that HOLDS a
/// company database is proof of company, so a daemon booting against a
/// registry with no row for it RESTORES the row and is admitted — loudly,
/// once. The incident behind it: the registry's own store was destroyed
/// externally under a live beacond, and the flat refusal this replaces left
/// a real company permanently unstartable, because nothing else could
/// recreate the row either. The empty directory above still refuses — the
/// proof is the database, never the binding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn a_company_database_is_proof_enough_to_restore_a_lost_registry_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = http_client();
    let beacond = spawn_beacond(dir.path(), &client).await;
    let company = company_directory(dir.path(), "orphaned");
    std::fs::create_dir_all(company.join(".chief").join("db")).expect("db dir");
    // An empty file IS a valid (empty) SQLite database — the daemon's own
    // schema-ensure fills it after admission. What this test exercises is
    // the PROOF-of-company check, and the proof is the file's existence at
    // the path only a company (or `chief rm`, which removes it before the
    // row) controls.
    std::fs::write(company.join(".chief").join("db").join("chief.db"), b"").expect("db file");

    let mut daemon = spawn_run(&company, dir.path(), "orphaned.log", free_port(), &beacond.url);
    let pid = daemon.child.id();
    wait_for_admission(&client, &beacond.url, &company, pid).await;
    let restored = lookup(&client, &beacond.url, &company).await;
    assert_eq!(
        restored["company"]["slug"],
        serde_json::json!("orphaned"),
        "the restored row carries the directory's basename as its display slug"
    );
    let log = daemon.read_log();
    assert!(
        log.contains("beacond.company_row.restored"),
        "a restored row must be LOUD — a registry that keeps losing rows is a live fault and \
         this line is how it gets found. Log:\n{log}"
    );
    send(pid, nix::sys::signal::Signal::SIGTERM);
    let _ = wait_for_exit(&mut daemon.child);

    // Second boot: the row exists, admission is ordinary, and the WARN does
    // NOT repeat — a restoration line on every boot would cry wolf about a
    // loss that happened once, and the wolf-crier is the guard that gets
    // deleted.
    let mut second = spawn_run(&company, dir.path(), "second.log", free_port(), &beacond.url);
    let second_pid = second.child.id();
    wait_for_admission(&client, &beacond.url, &company, second_pid).await;
    let second_log = second.read_log();
    assert!(
        !second_log.contains("beacond.company_row.restored"),
        "a second boot against an existing row must not claim a restoration. Log:\n{second_log}"
    );
    send(second_pid, nix::sys::signal::Signal::SIGTERM);
    let _ = wait_for_exit(&mut second.child);
}

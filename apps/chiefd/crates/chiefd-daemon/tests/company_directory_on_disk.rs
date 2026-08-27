//! **A company IS a directory: `chiefd run --dir <dir>` puts everything it
//! owns under `<dir>/.chief/`, and nothing anywhere else.**
//!
//! Real spawned `chiefd run` processes, real filesystem inspection: this stage
//! is a claim about what a real boot puts on disk, so a unit test against a
//! path builder alone cannot prove it — see `company_dir.rs`'s own test module
//! for the derivations. This file is the process-level proof.
//!
//! It replaces `per_company_database.rs`, whose subject — the dotfile sibling
//! `<data-root>/.<slug>.chief.db`, and the several ways one data root could
//! hold two companies — is deleted with the data root itself.
//!
//! It is a `tests/` integration test because it needs `CARGO_BIN_EXE_chiefd`
//! (cargo does not set it for the binary's own unit-test build).

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(30);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port").local_addr().unwrap().port()
}

fn health(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .write_all(b"GET /v1/docs/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

/// The company key. beacond records it verbatim and checks only its SHAPE, so
/// a test that registers a company must mint it the way the daemon does —
/// through the one shared definition, never a private copy of the hash.
use host_primitives::rendezvous::company_key;

/// The store a company directory holds. Stated as a literal here on purpose:
/// this file is the outside witness, so it must not derive the answer from the
/// same helper the daemon derives it from.
fn store_db_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join("db").join("chief.db")
}

/// The rendezvous a live daemon publishes, likewise stated literally.
fn rendezvous_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join("run").join("daemon.json")
}

/// A company directory under `parent`, created and ready to be served.
fn company_directory(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("company directory");
    dir
}

/// Every path under `root`, relative and recursive, sorted.
fn every_entry_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned());
        }
    }
    found.sort();
    found
}

/// Spawn `chiefd run --once` for one company directory and wait for it to
/// exit. `--once` runs the startup self-audit and one duty pass, then exits —
/// exactly enough to prove what got written to disk at boot, without standing
/// up a long-lived server this test would have to tear down. It never reaches
/// beacond admission, so it needs none.
fn run_once(
    dir: &Path,
    logs: &Path,
    extra_env: &[(&str, &str)],
    log_name: &str,
) -> (std::process::ExitStatus, String) {
    let log_path = logs.join(log_name);
    let log = std::fs::File::create(&log_path).expect("log file");
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"));
    command
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir)
        .arg("--launcher-root")
        .arg(logs)
        // A socket name no runtime server answers on: this suite is about what
        // lands on disk, not runtime actuation.
        .arg("--runtime-socket")
        .arg(format!("company-dir-{}-throwaway", company_key(dir)))
        .arg("--once")
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log));
    for (key, value) in extra_env {
        command.env(key, value);
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

/// E10-S3 (#764): a full (non-`--once`) `chiefd run` boot registers with
/// beacond immediately after bind, so any test that spawns one to prove
/// FULL-boot behaviour (not `run_once`'s startup-audit-then-exit shape, which
/// never reaches the docstore mount at all) needs a real beacond up and the
/// company pre-created in it. Sync raw-TCP HTTP, the same low-level style
/// `health()` above already uses, rather than pulling in an async runtime and
/// hyper for one helper.
struct Beacond {
    child: std::process::Child,
    port: u16,
}

impl Drop for Beacond {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn beacond_bin_path() -> PathBuf {
    let chiefd_bin = PathBuf::from(env!("CARGO_BIN_EXE_chiefd"));
    chiefd_bin.parent().expect("CARGO_BIN_EXE_chiefd has a parent directory").join("beacond")
}

fn http_request(port: u16, request: &str) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    Some(body)
}

fn spawn_beacond(dir: &Path) -> Beacond {
    let port = free_port();
    let db_path = dir.join("beacond.sqlite");
    let child = std::process::Command::new(beacond_bin_path())
        .env("BEACOND_BIND", format!("127.0.0.1:{port}"))
        .env("BEACOND_DB_PATH", &db_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("beacond spawns");
    // Wrapped immediately, before the readiness loop: `Beacond`'s `Drop`
    // owns `wait()`, and wrapping right after `spawn()` (rather than only
    // on the loop's success branch) is what lets that reach every exit
    // path, including this function's own panics.
    let beacond = Beacond { child, port };
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(response) = http_request(
            port,
            "GET /v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        ) {
            if response.starts_with("HTTP/1.1 200") {
                return beacond;
            }
        }
        assert!(Instant::now() < deadline, "beacond never became healthy on port {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn create_company(beacond_port: u16, dir: &Path, slug: &str) {
    let body =
        format!(r#"{{"dir":"{}","key":"{}","slug":"{slug}"}}"#, dir.display(), company_key(dir));
    let request = format!(
        "POST /v1/company/create HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response =
        http_request(beacond_port, &request).expect("company/create request reaches beacond");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "company/create for {} failed: {response}",
        dir.display()
    );
}

/// TWO DIRECTORIES MAY HOLD COMPANIES WITH THE SAME NAME, and each gets its
/// own store.
///
/// This is the case the retired layout could not represent at all: one slug
/// under one data root was one file, `.acme.chief.db`, so two companies called
/// `acme` on one box were one company. Both boots here are told nothing but a
/// directory, and neither knows the other exists.
#[test]
fn two_directories_holding_a_company_of_the_same_name_get_two_distinct_stores() {
    let dir = tempfile::tempdir().expect("tempdir");
    let here = company_directory(dir.path(), "here");
    let there = company_directory(dir.path(), "there");

    let (here_status, here_log) = run_once(&here, dir.path(), &[], "here.log");
    assert!(here_status.success(), "the first company's --once must succeed. Log:\n{here_log}");
    let (there_status, there_log) = run_once(&there, dir.path(), &[], "there.log");
    assert!(there_status.success(), "the second company's --once must succeed. Log:\n{there_log}");

    assert!(store_db_path(&here).is_file(), "each directory holds its own store");
    assert!(store_db_path(&there).is_file(), "each directory holds its own store");
    assert_ne!(
        company_key(&here),
        company_key(&there),
        "and each has its own identity on the wire"
    );

    // Neither store carries a row scoped to the other company's key — checked
    // against EVERY table that has a `slug` column rather than assumed from
    // the files merely having different paths.
    let conn = chiefd_core::store::open_company_db_readonly(&store_db_path(&here))
        .expect("open the first store");
    let foreign = company_key(&there);
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("prepare table list query");
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query table list")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect table list");
    assert!(!tables.is_empty(), "fixture: a freshly-schema'd store must have tables to check");
    for table in tables {
        // Not every table has a `slug` column (`counters`, `host_actions`,
        // `company_removal_completion_receipts`); skip silently where the
        // column does not exist rather than failing the query.
        let has_slug: bool = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = 'slug'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !has_slug {
            continue;
        }
        let rows: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{table}\" WHERE slug = ?1"),
                [&foreign],
                |row| row.get(0),
            )
            .unwrap_or_else(|error| panic!("count foreign rows in {table}: {error}"));
        assert_eq!(
            rows, 0,
            "the first company's {table} must contain zero rows scoped to the second company"
        );
    }
}

/// `CHIEFD_STORE_DB_PATH` present in the environment is IGNORED — boot with it
/// set to a decoy path and assert the decoy is never created while the
/// directory's own store is. Regression for the exact hazard a deploy
/// preflight could otherwise hit: verifying one file while the daemon opens
/// another.
#[test]
fn chiefd_store_db_path_in_the_environment_is_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = company_directory(dir.path(), "anvils");
    let decoy = dir.path().join("decoy-should-never-be-created.sqlite");

    let (status, log) = run_once(
        &company,
        dir.path(),
        &[("CHIEFD_STORE_DB_PATH", decoy.to_str().expect("utf8 path"))],
        "run.log",
    );
    assert!(status.success(), "--once must succeed even with a decoy env var set. Log:\n{log}");

    assert!(!decoy.exists(), "the decoy path named by CHIEFD_STORE_DB_PATH must NEVER be created");
    assert!(
        store_db_path(&company).is_file(),
        "the directory's own store must exist regardless of the decoy env var"
    );
}

/// **THE GOLDEN LISTING.** A fresh boot writes into `<dir>/.chief/` and
/// NOWHERE else.
///
/// The claim the whole stage rests on: `rm -rf <dir>` removes a company
/// completely, and a company writes nothing to the operator's home. Asserted
/// as a whole-tree walk rather than a handful of `exists()` checks, so a file
/// added later outside `.chief/` fails this test instead of passing review
/// unnoticed.
#[test]
fn a_fresh_boot_writes_inside_the_companys_own_chief_folder_and_nowhere_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = company_directory(dir.path(), "anvils");
    // The log goes OUTSIDE the company directory: it is this harness's
    // artifact, not something chiefd wrote, and leaving it inside would make
    // the listing below a statement about the test's own plumbing.
    let (status, log) = run_once(&company, dir.path(), &[], "run.log");
    assert!(status.success(), "--once must succeed. Log:\n{log}");

    let entries = every_entry_under(&company);
    for name in &entries {
        assert!(
            Path::new(name).starts_with(".chief"),
            "a fresh boot wrote {name:?} outside the company's own .chief folder. Full \
             listing: {entries:?}"
        );
    }
    assert!(
        entries.iter().any(|name| name == ".chief/db/chief.db"),
        "the store itself must be present. Full listing: {entries:?}"
    );
    assert!(
        !company.join(".pi").exists(),
        "the directory's own Pi config is the USER's; chiefd must not create it"
    );
}

/// **THE RENDEZVOUS.** A full boot publishes `<dir>/.chief/run/daemon.json`
/// naming this directory, this key, the URL that answers, and this pid — and a
/// graceful stop takes it away again.
///
/// This replaces a beacond lookup by slug on the attach path: the client that
/// wants this directory's daemon reads this file. Proven against a REAL boot
/// rather than the publish function alone, because the property is an
/// ORDERING one — the file must not appear until the URL in it answers.
#[test]
fn a_full_boot_publishes_a_rendezvous_that_answers_and_removes_it_on_a_clean_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let company = company_directory(dir.path(), "anvils");
    let beacond = spawn_beacond(dir.path());
    create_company(beacond.port, &company, "anvils");

    let port = free_port();
    let log_path = dir.path().join("run.log");
    let log = std::fs::File::create(&log_path).expect("log file");

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(&company)
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--runtime-socket")
        .arg("rendezvous-throwaway")
        .env("CHIEFD_STORE_BIND", format!("127.0.0.1:{port}"))
        .env("CHIEFD_STORE_PORT_WALK", "1")
        .env("BEACOND_URL", format!("http://127.0.0.1:{}", beacond.port))
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run spawns");

    let deadline = Instant::now() + DEADLINE;
    let response = loop {
        if let Some(response) = health(port) {
            break response;
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!(
                "chiefd run exited ({status:?}) before answering health. Log:\n{}",
                std::fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "chiefd run never answered /v1/docs/health within {DEADLINE:?}. Log:\n{}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the docstore surface must mount and answer 200: {response}"
    );

    // ORDERING: the surface answered, so the rendezvous must already be there
    // — it is published at the same latch, before the daemon serves a single
    // request. A poll here would hide a file that appears late.
    let path = rendezvous_path(&company);
    assert!(
        path.is_file(),
        "a daemon whose docstore answers must already have published {}. Log:\n{}",
        path.display(),
        std::fs::read_to_string(&log_path).unwrap_or_default()
    );
    let published: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the rendezvous"))
            .expect("the rendezvous is JSON");
    assert_eq!(
        published["dir"],
        serde_json::json!(company.display().to_string()),
        "it names the directory it was published for, so a copied file cannot mislead a client"
    );
    assert_eq!(published["key"], serde_json::json!(company_key(&company)));
    assert_eq!(published["pid"], serde_json::json!(child.id()));
    // THE URL IN THE FILE IS THE URL THAT ANSWERS. A pointer at an address
    // nobody serves is worse than no pointer at all.
    let url = published["url"].as_str().expect("the rendezvous carries a url").to_owned();
    assert_eq!(url, format!("http://127.0.0.1:{port}"), "published url: {url}");
    // AND IT SAYS WHICH BUILD IT IS RUNNING, measured by the daemon itself at
    // start. This is the end-to-end half of the version ensure: a client reads
    // this to answer "is the running daemon the installed build", and it can
    // only ever answer that if a REAL boot writes real values here.
    let build = &published["build"];
    assert_eq!(
        build["exe"],
        serde_json::json!(env!("CARGO_BIN_EXE_chiefd")),
        "the daemon reports the executable it was actually started from: {published}"
    );
    assert!(
        build["identity"]["ino"].as_u64().is_some_and(|ino| ino > 0),
        "a real inode, not a placeholder: {published}"
    );
    assert!(
        build["identity"]["size"].as_u64().is_some_and(|size| size > 0),
        "a real size: {published}"
    );
    assert!(
        build["identity"]["mtimeS"].as_i64().is_some_and(|mtime| mtime > 0),
        "a real modification time: {published}"
    );
    assert_eq!(
        published.as_object().expect("object").len(),
        5,
        "no field is unaccounted for: {published}"
    );

    // A GRACEFUL STOP TAKES IT AWAY, beside the beacond deregistration.
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).expect("pid fits")),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("SIGTERM is delivered");
    let status = {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never exited after SIGTERM. Log:\n{}",
                std::fs::read_to_string(&log_path).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let _ = status;
    assert!(
        !path.exists(),
        "a graceful shutdown must remove the rendezvous it published — a live-looking pointer \
         at a dead daemon costs every later command in this directory a probe. Log:\n{}",
        std::fs::read_to_string(&log_path).unwrap_or_default()
    );
}

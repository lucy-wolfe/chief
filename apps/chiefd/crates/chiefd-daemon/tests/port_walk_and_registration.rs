//! E10-S3 (#764): tests 11-14 from the story — `docstore-only`'s and
//! `--serve-only`'s deliberate NON-registration, the D20 boot-and-inspect
//! (no owner-marker file appears anywhere, ever, because the code that
//! wrote one no longer exists), and the D19 crash-between-bind-and-register
//! window (no half-admitted state to repair, because nothing was written
//! in two places).
//!
//! Same real-process discipline as `single_writer_admission.rs`: only real
//! spawned binaries can be killed for real or prove a genuine absence of a
//! network call.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post as axum_post;
use axum::Router;
use http_body_util::BodyExt as _;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;

const DEADLINE: Duration = Duration::from_secs(30);

type Client = HyperClient<HttpConnector, http_body_util::Full<hyper::body::Bytes>>;

enum BeaconProbeCommand {
    Snapshot(std::sync::mpsc::Sender<Vec<String>>),
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// A deliberately non-responsive local `BEACOND_URL` endpoint. Its recorder
/// starts before ChiefD and is joined on every ordinary or unwinding exit, so
/// an attempted connection cannot disappear merely because the peer timed out
/// or closed before the fixture inspected the listener.
struct BeaconContactRecorder {
    url: String,
    commands: std::sync::mpsc::Sender<BeaconProbeCommand>,
    stopping: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl BeaconContactRecorder {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind beacon probe");
        listener.set_nonblocking(true).expect("make beacon probe nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("beacon probe address"));
        let contacts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stopping = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (commands, command_rx) = std::sync::mpsc::channel();
        let recorder_contacts = std::sync::Arc::clone(&contacts);
        let recorder_stopping = std::sync::Arc::clone(&stopping);
        let task = std::thread::spawn(move || {
            loop {
                drain_beacon_probe_contacts(&listener, &recorder_contacts);
                if recorder_stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }

                match command_rx.try_recv() {
                    Ok(BeaconProbeCommand::Snapshot(response)) => {
                        // This is a recording barrier: include every queued
                        // connection before reporting the immutable history.
                        drain_beacon_probe_contacts(&listener, &recorder_contacts);
                        let snapshot =
                            recorder_contacts.lock().expect("read beacon probe contacts").clone();
                        let _ = response.send(snapshot);
                    }
                    Ok(BeaconProbeCommand::Shutdown(response)) => {
                        drain_beacon_probe_contacts(&listener, &recorder_contacts);
                        let _ = response.send(());
                        return;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }
        });

        Self { url, commands, stopping, task: Some(task) }
    }

    fn assert_no_contacts(&self, phase: &str) {
        let (response, snapshot) = std::sync::mpsc::channel();
        self.commands
            .send(BeaconProbeCommand::Snapshot(response))
            .expect("ask beacon probe recorder for its contact history");
        let contacts = snapshot
            .recv_timeout(Duration::from_secs(1))
            .expect("beacon probe recorder did not answer its contact snapshot");
        assert!(
            contacts.is_empty(),
            "docstore-only contacted its held BEACOND_URL probe {phase}: {contacts:#?}"
        );
    }

    fn shutdown(&mut self) {
        let (response, finished) = std::sync::mpsc::channel();
        self.commands
            .send(BeaconProbeCommand::Shutdown(response))
            .expect("ask beacon probe recorder to stop");
        finished
            .recv_timeout(Duration::from_secs(1))
            .expect("beacon probe recorder did not acknowledge shutdown");
        self.task
            .take()
            .expect("beacon probe recorder is still owned")
            .join()
            .expect("join beacon probe recorder");
    }
}

impl Drop for BeaconContactRecorder {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        self.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = task.join();
    }
}

fn drain_beacon_probe_contacts(
    listener: &std::net::TcpListener,
    contacts: &std::sync::Mutex<Vec<String>>,
) {
    loop {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                stream
                    .set_nonblocking(true)
                    .expect("make accepted beacon probe stream nonblocking");
                let mut request = [0_u8; 4096];
                let request = match stream.read(&mut request) {
                    Ok(bytes) => String::from_utf8_lossy(&request[..bytes]).into_owned(),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        "<request bytes still pending at accept>".to_string()
                    }
                    Err(error) => format!("<request read failed: {error}>"),
                };
                contacts
                    .lock()
                    .expect("record beacon probe contact")
                    .push(format!("peer={peer} request={request:?}"));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return,
            Err(error) => {
                contacts
                    .lock()
                    .expect("record beacon probe listener error")
                    .push(format!("<beacon probe accept failed: {error}>"));
                return;
            }
        }
    }
}

#[derive(Debug)]
struct RegisterObservation {
    url: String,
    port: u16,
    db_exists: bool,
    wal_exists: bool,
    shm_exists: bool,
}

#[derive(Clone)]
struct AdmissionProbe {
    db_path: PathBuf,
    observed: std::sync::Arc<
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<RegisterObservation>>>,
    >,
    release: std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

async fn probe_register(
    State(probe): State<AdmissionProbe>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let observation = RegisterObservation {
        url: body["url"].as_str().unwrap_or_default().to_string(),
        port: body["port"].as_u64().and_then(|port| u16::try_from(port).ok()).unwrap_or_default(),
        db_exists: probe.db_path.exists(),
        wal_exists: PathBuf::from(format!("{}-wal", probe.db_path.display())).exists(),
        shm_exists: PathBuf::from(format!("{}-shm", probe.db_path.display())).exists(),
    };
    if let Some(sender) = probe.observed.lock().await.take() {
        let _ = sender.send(observation);
    }
    let release = probe.release.lock().await.take();
    if let Some(release) = release {
        let _ = release.await;
    }
    (StatusCode::OK, Json(serde_json::json!({"registered": true}))).into_response()
}

async fn probe_ok() -> Response {
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn spawn_admission_probe(
    db_path: PathBuf,
) -> (
    String,
    tokio::sync::oneshot::Receiver<RegisterObservation>,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let probe = AdmissionProbe {
        db_path,
        observed: std::sync::Arc::new(tokio::sync::Mutex::new(Some(observed_tx))),
        release: std::sync::Arc::new(tokio::sync::Mutex::new(Some(release_rx))),
    };
    let app = Router::new()
        .route("/v1/register", axum_post(probe_register))
        .route("/v1/heartbeat", axum_post(probe_ok))
        .route("/v1/deregister", axum_post(probe_ok))
        .with_state(probe);
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind admission probe");
    let addr = listener.local_addr().expect("admission probe addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve admission probe");
    });
    (format!("http://{addr}"), observed_rx, release_tx, server)
}

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

/// Own a real child immediately after it is spawned. If an assertion aborts a
/// test before its ordinary shutdown, `Drop` still kills and reaps the child
/// and prints the same log that made readiness diagnosable.
struct ChildGuard {
    label: &'static str,
    child: std::process::Child,
    log_path: PathBuf,
    stopped: bool,
}

impl ChildGuard {
    fn new(label: &'static str, child: std::process::Child, log_path: PathBuf) -> Self {
        Self { label, child, log_path, stopped: false }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn read_log(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log_path).unwrap_or_default())
    }

    fn assert_running(&mut self) {
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!(
                "{} exited before readiness ({status}). Log:\n{}",
                self.label,
                self.read_log()
            ),
            Err(error) => panic!(
                "could not inspect {} before readiness ({error}). Log:\n{}",
                self.label,
                self.read_log()
            ),
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        self.child.wait().expect("reap guarded child");
        self.stopped = true;
    }

    fn mark_reaped(&mut self) {
        self.stopped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }

        let log = self.read_log();
        let _ = self.child.kill();
        let _ = self.child.wait();
        eprintln!(
            "{} was still owned during early test exit; killed and reaped it. Captured log:\n{log}",
            self.label
        );
    }
}

struct Beacond {
    child: ChildGuard,
    url: String,
}

impl Beacond {
    fn stop(&mut self) {
        self.child.stop();
    }
}

/// See `single_writer_admission.rs`'s identical helper's doc comment:
/// `CARGO_BIN_EXE_beacond` is not populated for this cross-package
/// dependency in this workspace, so both binaries' shared target directory
/// is used instead. The `beacond` dev-dependency in `Cargo.toml` still
/// guarantees the binary is actually built before this test runs.
fn beacond_bin_path() -> PathBuf {
    let chiefd_bin = PathBuf::from(env!("CARGO_BIN_EXE_chiefd"));
    chiefd_bin.parent().expect("CARGO_BIN_EXE_chiefd has a parent directory").join("beacond")
}

async fn spawn_beacond(dir: &Path, client: &Client) -> Beacond {
    let db_path = dir.join("beacond.sqlite");
    let log_path = dir.join("beacond.log");
    let log = std::fs::File::create(&log_path).expect("beacond log file");
    let child = std::process::Command::new(beacond_bin_path())
        .env("BEACOND_BIND", "127.0.0.1:0")
        .env("BEACOND_DB_PATH", &db_path)
        .stdout(std::process::Stdio::from(log.try_clone().expect("duplicate beacond log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("beacond spawns");
    let mut child = ChildGuard::new("beacond", child, log_path);
    let url =
        wait_for_bound_health(client, &mut child, "beacond listening", "/v1/health", DEADLINE)
            .await;
    Beacond { child, url }
}

/// The company key. beacond records it verbatim and checks only its SHAPE, so
/// a test that registers a company must mint it the way the daemon does —
/// through the one shared definition, never a private copy of the hash.
use host_primitives::rendezvous::company_key;

/// The store a company directory holds, `<dir>/.chief/db/chief.db`.
fn store_db_path(dir: &Path) -> PathBuf {
    dir.join(".chief").join("db").join("chief.db")
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

fn strip_ansi(raw: &str) -> String {
    let mut clean = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            clean.push(character);
        }
    }
    clean
}

fn bound_url_from_log(log: &str, message: &str) -> Option<String> {
    strip_ansi(log).lines().rev().filter(|line| line.contains(message)).find_map(|line| {
        line.split_ascii_whitespace()
            .find_map(|field| field.strip_prefix("bind="))
            .map(|bind| format!("http://{}", bind.trim_matches('"')))
    })
}

async fn wait_for_bound_health(
    client: &Client,
    child: &mut ChildGuard,
    message: &str,
    health_path: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let log = child.read_log();
        if let Some(url) = bound_url_from_log(&log, message) {
            if let Some((200, _)) = try_get(client, &format!("{url}{health_path}")).await {
                child.assert_running();
                return url;
            }
        }
        child.assert_running();
        assert!(
            Instant::now() < deadline,
            "{} never logged a healthy listener ({message}). Log:\n{}",
            child.label,
            child.read_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn readiness_uses_the_actual_bound_endpoint_from_the_listener_log() {
    let log = "\u{1b}[2mINFO beacond: beacond listening bind=127.0.0.1:49271 db=/tmp/beacond.sqlite\u{1b}[0m";
    assert_eq!(
        bound_url_from_log(log, "beacond listening"),
        Some("http://127.0.0.1:49271".to_string())
    );
}

/// Reserve a base port only when its immediate successor is also available.
/// The test below needs a deterministic one-step walk, not merely any free
/// ephemeral port; retrying a bounded number of times avoids a false failure
/// when another process happens to own `base + 1`.
fn held_port_with_free_successor() -> (std::net::TcpListener, u16) {
    for _ in 0..128 {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold base port");
        let base = held.local_addr().expect("base addr").port();
        let Some(successor) = base.checked_add(1) else {
            continue;
        };
        match std::net::TcpListener::bind(("127.0.0.1", successor)) {
            Ok(free_successor) => {
                drop(free_successor);
                return (held, base);
            }
            Err(_) => drop(held),
        }
    }
    panic!("could not find a held port with an available immediate successor");
}

fn wait_for_exit(child: &mut ChildGuard) -> std::process::ExitStatus {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.child.try_wait().expect("try_wait") {
            child.mark_reaped();
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "{} never exited. Log:\n{}",
            child.label,
            child.read_log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---- Test 10: normal run reserves -> registers -> opens both stores ------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn admitted_daemon_registers_its_reserved_walked_listener_before_either_sqlite_surface_opens()
{
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = store_db_path(dir.path());
    let (probe_url, observed, release, server) = spawn_admission_probe(db_path.clone()).await;
    let (held_base, base_port) = held_port_with_free_successor();
    let expected_port = base_port.checked_add(1).expect("successor selected above");
    let log_path = dir.path().join("admission-order.log");
    let log = std::fs::File::create(&log_path).expect("log file");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--runtime-socket")
        .arg("admission-order-throwaway")
        .env("CHIEFD_STORE_BIND", format!("127.0.0.1:{base_port}"))
        .env("CHIEFD_STORE_PORT_WALK", "2")
        .env("BEACOND_URL", &probe_url)
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run spawns");
    let mut child = ChildGuard::new("admission-order chiefd", child, log_path);

    // The probe withholds its 200 until this assertion completes. Thus the
    // child cannot move past `register`; a missing common SQLite file and its
    // sidecars prove that neither `CompanyDb::open` nor `DocStore::open` ran
    // before admission succeeded.
    let observation = tokio::time::timeout(DEADLINE, observed)
        .await
        .expect("chiefd did not issue its one register call before the deadline")
        .expect("admission probe retained its observation sender");
    assert_eq!(observation.port, expected_port, "the bounded walk must skip only the held base");
    assert_eq!(
        observation.url,
        format!("http://127.0.0.1:{expected_port}"),
        "beacond must receive the exact listener address reserved by the walk"
    );
    assert!(
        !observation.db_exists && !observation.wal_exists && !observation.shm_exists,
        "register arrived after company/docstore storage had already opened: {observation:?}"
    );

    release.send(()).expect("release the admitted daemon");
    let client = http_client();
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some((200, _)) =
            try_get(&client, &format!("{}/v1/docs/health", observation.url)).await
        {
            break;
        }
        child.assert_running();
        assert!(
            Instant::now() < deadline,
            "the registered listener never mounted the docstore at {}",
            observation.url
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(db_path.exists(), "the admitted path must open the shared company/docstore database");

    child.stop();
    drop(held_base);
    server.abort();
}

// ---- Test 11: a non-registering mount never contacts its beacon probe ----
//
// The mount driven here used to be `chiefd docstore-only`. That mode is
// deleted, so the claim now rides on the surviving non-registering mount,
// `chiefd run --serve-only`: it returns from `run` before beacond admission is
// even reached, so a held BEACOND_URL must record zero contacts for the whole
// child lifetime. Test 12 below proves the weaker registry-level fact (the
// writer's row is untouched); this one proves no packet is sent at all.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_only_boots_and_stops_cleanly_without_contacting_its_beacon_probe() {
    // Start the recorder before ChiefD. It accepts and records every forbidden
    // TCP contact for the entire child lifetime, rather than treating a later
    // empty accept queue as evidence that a timed-out peer never connected.
    let dir = tempfile::tempdir().expect("tempdir");
    let client = http_client();
    let mut beacon_probe = BeaconContactRecorder::spawn();
    let log_path = dir.path().join("serve-only.log");
    let log = std::fs::File::create(&log_path).expect("log file");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--serve-only")
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("BEACOND_URL", &beacon_probe.url)
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run --serve-only spawns");
    let mut child = ChildGuard::new("serve-only chiefd", child, log_path);

    let _ = wait_for_bound_health(
        &client,
        &mut child,
        "chiefd org_documents store surface listening",
        "/v1/docs/health",
        Duration::from_secs(5),
    )
    .await;

    beacon_probe.assert_no_contacts("while its real docstore health endpoint is live");
    child.stop();
    beacon_probe.assert_no_contacts("after ChildGuard reaped the snapshot reader");
    beacon_probe.shutdown();
}

// ---- Test 12: --serve-only registers nothing, is refused nothing ----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn serve_only_registers_nothing_and_the_writers_location_is_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = http_client();
    let mut beacond = spawn_beacond(dir.path(), &client).await;
    create_company(&client, &beacond.url, dir.path(), "acme").await;

    let writer_log = dir.path().join("writer.log");
    let writer_output = std::fs::File::create(&writer_log).expect("writer log");
    let writer_child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--runtime-socket")
        .arg("serve-only-scoping-throwaway")
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("CHIEFD_STORE_PORT_WALK", "1")
        .env("BEACOND_URL", &beacond.url)
        .stdout(std::process::Stdio::from(writer_output.try_clone().expect("duplicate writer log")))
        .stderr(std::process::Stdio::from(writer_output))
        .spawn()
        .expect("writer spawns");
    let mut writer = ChildGuard::new("writer chiefd", writer_child, writer_log);
    let writer_pid = writer.id();

    let deadline = Instant::now() + DEADLINE;
    loop {
        let company = lookup(&client, &beacond.url, dir.path()).await;
        if company["company"]["pid"] == serde_json::json!(i64::from(writer_pid)) {
            break;
        }
        writer.assert_running();
        assert!(Instant::now() < deadline, "the writer never registered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let writer_url = wait_for_bound_health(
        &client,
        &mut writer,
        "chiefd org_documents store surface listening",
        "/v1/docs/health",
        DEADLINE,
    )
    .await;

    // --serve-only for the SAME company: must not touch beacond at all.
    let serve_only_log = dir.path().join("serve-only.log");
    let serve_only_output = std::fs::File::create(&serve_only_log).expect("serve-only log");
    let serve_only_child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--runtime-socket")
        .arg("serve-only-scoping-throwaway-reader")
        .arg("--serve-only")
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("CHIEFD_STORE_PORT_WALK", "1")
        .env("BEACOND_URL", &beacond.url)
        .stdout(std::process::Stdio::from(
            serve_only_output.try_clone().expect("duplicate serve-only log"),
        ))
        .stderr(std::process::Stdio::from(serve_only_output))
        .spawn()
        .expect("serve-only spawns");
    let mut serve_only = ChildGuard::new("serve-only chiefd", serve_only_child, serve_only_log);
    let _ = wait_for_bound_health(
        &client,
        &mut serve_only,
        "chiefd org_documents store surface listening",
        "/v1/docs/health",
        DEADLINE,
    )
    .await;

    // beacond's row for this directory still names the WRITER's pid, unchanged.
    let after = lookup(&client, &beacond.url, dir.path()).await;
    assert_eq!(
        after["company"]["pid"],
        serde_json::json!(i64::from(writer_pid)),
        "--serve-only must never overwrite the real writer's registered location"
    );
    assert_eq!(
        after["company"]["url"],
        serde_json::json!(writer_url),
        "--serve-only must leave the real writer's logged listener location unchanged"
    );

    serve_only.stop();
    writer.stop();
    beacond.stop();
}

// ---- Test 13: D20 boot-and-inspect — no owner-marker file, ever ----

#[test]
fn a_once_boot_leaves_no_owner_marker_file_anywhere_in_the_company_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log_path = dir.path().join("once.log");
    let log = std::fs::File::create(&log_path).expect("log file");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--once")
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run --once spawns");
    let mut child = ChildGuard::new("one-shot chiefd", child, log_path);
    let status = wait_for_exit(&mut child);
    // `--once` against a company with no manifest may itself refuse (there
    // is nothing to converge) -- irrelevant to this test, which asserts on
    // DISK STATE regardless of exit status.
    let _ = status;

    // RECURSIVE, not a top-level listing: everything chiefd writes now lives
    // one level down under `<dir>/.chief/`, so scanning only the directory's
    // own entries would find nothing and prove nothing.
    let entries = every_entry_under(dir.path());
    assert!(
        !entries.iter().any(|name| name.contains("chiefd-owner")),
        "no owner-marker file may appear anywhere under the company directory -- the module \
         that wrote it is deleted. Entries: {entries:?}"
    );
    assert!(
        !entries.iter().any(|name| name.ends_with(".json.tmp")),
        "no stale claim-protocol temp file may appear either. Entries: {entries:?}"
    );
    // And the positive control, without which both assertions above would pass
    // on a boot that wrote nothing at all: `--once` really did open the store.
    assert!(
        entries.iter().any(|name| name.ends_with("chief.db")),
        "positive control failed: --once left no store behind, so this test could not have \
         detected a marker file. Entries: {entries:?}"
    );
}

/// Every path under `root`, relative and recursive.
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
    found
}

// ---- Test 14: D19 — no half-admitted state between bind and register ----
//
// This held HTTP probe verifies only the pre-admission storage-ordering
// boundary. It deliberately never commits a real beacond register; the
// actual-registry crash/reopen atomicity remains covered by
// `beacond/tests/crash_mid_transaction.rs`'s
// `a_crash_mid_register_leaves_the_previous_location_intact`, which preserves
// the complete prior row across a dropped real SQLite transaction.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn a_daemon_killed_between_bind_and_register_leaves_no_half_admitted_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = store_db_path(dir.path());
    let (probe_url, observed, release, server) = spawn_admission_probe(db_path.clone()).await;
    let log_path = dir.path().join("crash-window.log");
    let log = std::fs::File::create(&log_path).expect("log file");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
        .arg("run")
        // `chiefd run` no longer guesses what a pane execs; the operator
        // client resolves it absolutely and passes it. See
        // `parse_config_refuses_a_daemon_that_was_not_told_what_panes_exec`.
        .arg("--pi-binary")
        .arg("/opt/pi/bin/pi")
        .arg("--dir")
        .arg(dir.path())
        .arg("--launcher-root")
        .arg(dir.path())
        .arg("--runtime-socket")
        .arg("crash-window-throwaway")
        .env("CHIEFD_STORE_BIND", "127.0.0.1:0")
        .env("CHIEFD_STORE_PORT_WALK", "1")
        .env("BEACOND_URL", &probe_url)
        .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .expect("chiefd run spawns");
    let mut child = ChildGuard::new("crash-window chiefd", child, log_path);

    // The probe observes the register request and withholds its 200. That
    // pins the child after its real ephemeral listener was reserved but before
    // registration can complete, without reopening a guessed port in the
    // fixture. Retaining `release` keeps the handler blocked through the kill.
    let observation = tokio::time::timeout(DEADLINE, observed)
        .await
        .expect("the daemon never reached the held admission boundary")
        .expect("admission probe retained its observation sender");
    assert!(
        observation.port > 0 && observation.url == format!("http://127.0.0.1:{}", observation.port),
        "the held registration must carry the kernel-selected listener address: {observation:?}"
    );
    assert!(
        !observation.db_exists && !observation.wal_exists && !observation.shm_exists,
        "the daemon opened storage before the held admission could succeed: {observation:?}"
    );

    child.stop();
    drop(release);
    server.abort();
    assert!(
        !db_path.exists()
            && !PathBuf::from(format!("{}-wal", db_path.display())).exists()
            && !PathBuf::from(format!("{}-shm", db_path.display())).exists(),
        "killing at the held pre-admission boundary must leave no post-admission storage"
    );
}

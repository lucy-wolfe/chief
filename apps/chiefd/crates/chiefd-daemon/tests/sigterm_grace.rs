//! **`chiefd run` must honour SIGTERM well inside its supervisor's grace — on
//! every restart, not on a lucky one.**
//!
//! Measured on cobalt 2026-07-24: four restarts, four
//! `SIGTERM did not stop pid <N> within 10s — escalating to SIGKILL`, zero
//! clean shutdowns. `scripts/promote-chiefd.sh` escalates correctly, so no
//! deploy ever *failed* — which is precisely why it read as intermittent. It
//! was 4-for-4. Every chiefd restart in this system's history took the crash
//! path, so nothing that depends on orderly shutdown (duty-pass drain, lease
//! release, `wal_checkpoint(TRUNCATE)`, listener release) had ever been
//! exercised as designed.
//!
//! The cause reproduced here: `axum`'s graceful shutdown waits for every
//! in-flight **connection**, and the retained normalized-changefeed route
//! `GET /v1/docs/watch` is an SSE stream that by construction never completes.
//! Generic document reads and writes are retired; that survivor is why the
//! mount still needs a watcher-specific shutdown path. With one live watcher —
//! production always has several — `Daemon::serve`'s `docstore_task.await`
//! timed out and aborted the listener. So this test holds a real watch stream
//! open across the signal and proves clean EOF plus `docstore_drained=true`;
//! without both, a quick exit could merely be the old abort path.
//!
//! It is a `tests/` integration test because it needs `CARGO_BIN_EXE_chiefd`
//! (cargo does not set it for the binary's own unit-test build) and because
//! only a real process can be sent a real signal.
//!
//! It restarts the SAME company against the SAME `org.sqlite` five consecutive
//! times: a path that has failed 4-for-4 in production is not believed on the
//! evidence of one success.

// Real second process, real kernel signal, real wall-clock grace: the
// separate-process exception to the injected-clock rule (`chiefd_core::clock`).
// `std::fs::File` is allowed here for the same reason: the child's log is a
// test artifact this process reads back, not a chiefd-owned file handle.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chiefd_core::store::identities::{Identity, IdentityKind};
use p256::pkcs8::{EncodePrivateKey as _, EncodePublicKey as _, LineEnding};
use p256::SecretKey;

/// The supervisor's grace: `scripts/promote-chiefd.sh` waits this long before
/// escalating to SIGKILL. The daemon's own drain budget is deliberately well
/// under it (see `run::SHUTDOWN_BUDGET`) — the fix is a bounded drain, never a
/// longer grace.
const SUPERVISOR_GRACE: Duration = Duration::from_secs(10);

/// How many consecutive restarts must succeed.
const RESTARTS: usize = 5;

#[test]
#[cfg(unix)]
fn sigterm_drains_within_the_supervisor_grace_across_repeated_restarts() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The company is a directory NESTED inside the tempdir, so this harness's
    // own log files sit beside it rather than inside the tree the company owns
    // — the daemon's trust anchor now lives at `<dir>/.chief/keys`, and a
    // company rooted at the shared system temp directory would put it where
    // every other test's would land too.
    let company = dir.path().join("sigterm-grace");
    std::fs::create_dir_all(&company).expect("company directory");
    // The bearer this test's watch stream presents. `/v1/docs/watch` is not
    // exempt and must not become so — an SSE connection held across the signal
    // is the whole subject here, so this cannot be re-pointed at a credential-
    // free route the way a plain liveness probe could be.
    let bearer = provision_operator_bearer(&company);

    // E10-S3 (#764): a full boot now registers with beacond immediately
    // after bind, so this test needs a real one up and the company
    // pre-created — once, since every restart below reuses the SAME
    // company against the SAME data root. Each restart's `register` call
    // reclaims the location fresh (the previous restart's clean shutdown
    // already deregistered it; a #764 property in its own right, not
    // re-proven here).
    let beacond = spawn_beacond(dir.path());
    create_company(beacond.port, &company, "sigterm-grace");
    let beacond_url = format!("http://127.0.0.1:{}", beacond.port);

    for restart in 1..=RESTARTS {
        let port = free_port();
        let log_path = dir.path().join(format!("chiefd-{restart}.log"));
        let log = std::fs::File::create(&log_path).expect("log file");

        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_chiefd"))
            .arg("run")
            // `chiefd run` no longer guesses what a pane execs; the operator
            // client resolves it absolutely and passes it.
            .arg("--pi-binary")
            .arg("/opt/pi/bin/pi")
            .arg("--dir")
            .arg(&company)
            .arg("--launcher-root")
            .arg(dir.path())
            // A socket name no runtime server answers on: this test is about the
            // shutdown path, and it must never touch an operator's panes.
            .arg("--runtime-socket")
            .arg("sigterm-grace-throwaway")
            // No shared-store env var (E10-S2, #763): `chiefd run` resolves
            // its own per-company database now.
            .env("CHIEFD_STORE_BIND", format!("127.0.0.1:{port}"))
            .env("CHIEFD_STORE_PORT_WALK", "1")
            .env("BEACOND_URL", &beacond_url)
            .stdout(std::process::Stdio::from(log.try_clone().expect("dup log")))
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .expect("chiefd run spawns");

        let mut watcher = open_watch_stream(port, restart, &bearer);

        let signalled = Instant::now();
        send_sigterm(child.id());
        let status = wait_within(&mut child, SUPERVISOR_GRACE).unwrap_or_else(|| {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "restart {restart}/{RESTARTS}: chiefd did not exit within the {:?} supervisor \
                 grace after SIGTERM — this is the SIGKILL escalation, reproduced. Log:\n{}",
                SUPERVISOR_GRACE,
                std::fs::read_to_string(&log_path).unwrap_or_default()
            )
        });
        let elapsed = signalled.elapsed();

        // A mounted watcher receives a normal EOF before the listener drains.
        // Aborting the listener can also eventually close a TCP socket, so this
        // evidence is deliberately paired with the explicit clean-drain log
        // assertions below.
        assert_watch_stream_ended(&mut watcher, restart);

        assert!(
            status.success(),
            "restart {restart}/{RESTARTS}: chiefd exited unsuccessfully ({status:?}) after SIGTERM"
        );

        // The drain must be *observed*, not inferred from a quick exit: a
        // process that exits fast because it aborted everything is exactly the
        // failure this test exists to prevent. `duties_drained=true` is written
        // on the cooperative path only.
        let log = strip_ansi(&std::fs::read_to_string(&log_path).expect("read log"));
        assert!(
            log.contains("shutdown signal received; draining in-flight duty passes"),
            "restart {restart}/{RESTARTS}: the signal handler was never reached"
        );
        assert!(
            log.contains("chiefd run: stopped"),
            "restart {restart}/{RESTARTS}: chiefd exited without completing its drain"
        );
        assert!(
            log.contains("duties_drained=true"),
            "restart {restart}/{RESTARTS}: in-flight duty passes were abandoned rather than \
             drained. Log:\n{log}"
        );
        assert!(
            log.contains("docstore_drained=true"),
            "restart {restart}/{RESTARTS}: the mounted watcher held the docstore drain open or the listener was aborted. Log:\n{log}"
        );
        assert!(
            !log.contains("docstore drain exceeded its budget") && !log.contains("aborting the listener"),
            "restart {restart}/{RESTARTS}: clean watcher EOF regressed to listener abort. Log:\n{log}"
        );
        assert!(
            log.contains("writer_drained=true"),
            "restart {restart}/{RESTARTS}: the writer was not quiesced/checkpointed on the way \
             out. Log:\n{log}"
        );
        assert!(
            elapsed < SUPERVISOR_GRACE,
            "restart {restart}/{RESTARTS}: drain took {elapsed:?}, past the {SUPERVISOR_GRACE:?} \
             grace"
        );
    }
}

/// Read the remainder after SIGTERM. A clean watcher EOF is required before
/// the process can claim a drained listener; this is bounded by the socket's
/// ten-second read deadline installed in [`open_watch_stream`].
fn assert_watch_stream_ended(socket: &mut TcpStream, restart: usize) {
    let mut remainder = Vec::new();
    socket.read_to_end(&mut remainder).unwrap_or_else(|error| {
        panic!("restart {restart}: watcher did not reach EOF after shutdown: {error}")
    });
}

/// Connect to the mounted docstore and start a `/v1/docs/watch` SSE stream,
/// returning the still-open socket. Held across the signal on purpose: it is
/// the in-flight connection `axum`'s graceful shutdown would otherwise wait on
/// forever. Also serves as the readiness gate — the surface is up exactly when
/// it answers this.
fn open_watch_stream(port: u16, restart: usize, bearer: &str) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(mut socket) = TcpStream::connect(("127.0.0.1", port)) {
            socket.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
            let request = format!(
                "GET /v1/docs/watch?slug=sigterm-grace&stores=organization HTTP/1.1\r\n\
                 Host: 127.0.0.1\r\nAccept: text/event-stream\r\n\
                 Authorization: Bearer {bearer}\r\n\r\n"
            );
            if socket.write_all(request.as_bytes()).is_ok() {
                let mut head = [0_u8; 15];
                if socket.read_exact(&mut head).is_ok() {
                    assert!(
                        head.starts_with(b"HTTP/1.1 200"),
                        "restart {restart}: the watch stream was refused: {}",
                        String::from_utf8_lossy(&head)
                    );
                    return socket;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "restart {restart}: the docstore surface never came up on :{port}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Provision the daemon's trust anchor before it boots, and mint the operator
/// bearer this test's watch stream presents.
///
/// # Why the test provisions rather than reads back
///
/// `chiefd run` creates `<dir>/.chief/keys/operator.key` at boot and PRESERVES an
/// existing one, and it reads a provisioned `auth-hs256.secret` in preference to
/// minting an ephemeral in-process one. Writing both here therefore makes the
/// whole credential deterministic and offline: no polling for a file the daemon
/// is about to write, no challenge round trip, and — because the secret is
/// stable across restarts — ONE token that stays valid for all five boots, which
/// is what keeps this test about SIGTERM rather than about token lifetimes.
///
/// The scalar is fixed for the same reason every other fixture key in this tree
/// is: a deterministic key needs no RNG feature and reproduces byte for byte.
fn provision_operator_bearer(dir: &Path) -> String {
    // `<dir>/.chief/keys`, derived exactly as `company_dir::keys_dir` derives
    // it. If the two ever disagreed the daemon would mint its own anchor and
    // this test would be about a credential nothing reads.
    let keys = identity_keys::keys_dir(&dir.join(".chief"));
    std::fs::create_dir_all(&keys).expect("keys dir");

    let secret = [11_u8; 32];
    std::fs::write(identity_keys::hs256_secret_path(&keys), secret).expect("provision secret");

    let key = SecretKey::from_slice(&[7_u8; 32]).expect("scalar");
    let pem = key.to_pkcs8_pem(LineEnding::LF).expect("pem");
    let key_path = identity_keys::operator_key_path(&keys);
    std::fs::write(&key_path, pem.as_bytes()).expect("write operator key");
    // 0600 or the daemon refuses to serve at all (#1092): a private key anyone
    // can read is a key to assume is copied.
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let spki = key.public_key().to_public_key_der().expect("spki");
    let fingerprint = chiefd_api::authn::fingerprint_of_spki(spki.as_bytes());
    let identity = Identity {
        identity_id: identity_keys::OPERATOR_IDENTITY_ID.to_owned(),
        principal: identity_keys::OPERATOR_IDENTITY_ID.to_owned(),
        kind: IdentityKind::Operator,
        company_slug: None,
        pubkey: None,
        fingerprint,
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    };
    chiefd_api::authn::issue_token_for(&secret, &identity, 0).expect("mint the operator bearer")
}

/// Wait for the child, giving up after `grace`. `Some(status)` means it exited
/// on its own.
fn wait_within(
    child: &mut std::process::Child,
    grace: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A port nothing is listening on right now. The kernel hands out ephemeral
/// ports far from where it just was, so the reuse window is not a practical
/// race — and a taken port would make the daemon refuse loudly at startup, not
/// pass silently.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port").local_addr().unwrap().port()
}

/// E10-S3 (#764): a real spawned beacond, killed on drop. Sync raw-TCP HTTP
/// (same low-level style this file's `open_watch_stream` already uses)
/// rather than pulling in an async runtime + hyper for two calls.
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
    let deadline = Instant::now() + Duration::from_secs(30);
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

/// The company key. beacond records it verbatim and checks only its SHAPE, so
/// a test that registers a company must mint it the way the daemon does —
/// through the one shared definition, never a private copy of the hash.
use host_primitives::rendezvous::company_key;

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
    assert!(response.starts_with("HTTP/1.1 200"), "company/create for {slug} failed: {response}");
}

/// `tracing` colours the `=` in `field=value`, so an un-stripped log makes
/// `contains("duties_drained=true")` match nothing — the trap that has cost
/// hours of grepping.
fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// The real signal the supervisor sends — not `Child::kill` (SIGKILL), which
/// would test nothing.
fn send_sigterm(pid: u32) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).expect("a pid fits in i32")),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("SIGTERM is delivered");
}

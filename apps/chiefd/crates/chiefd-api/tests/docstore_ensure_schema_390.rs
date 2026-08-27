//! #390 — chiefd's docstore reaches its `200 ok` health contract on a brand-new
//! store file WITHOUT any external write, by ensuring its own schema at startup.
//!
//! The boot-flow deadlock this locks down: the launcher health-gates on
//! `200 {"status":"ok"}` before it performs the first write, but the docstore's
//! health is `503 schema-missing` until the schema exists — and pre-#390 the
//! schema was only ever created lazily (first write / an out-of-band
//! `POST /v1/docs/ensure-schema`). So boot could never pass: a perfectly healthy
//! daemon was SIGKILLed at the 15s gate timeout.
//!
//! These tests drive the EXACT production serving path `chiefd run` uses —
//! `docstore::bind` → `Bound::ensure_schema` → `docstore::serve_bound` — over a
//! real loopback socket, proving both the fix and the pre-fix deadlock state.

// In a `tests/` integration binary `cfg(test)` is not set, so the workspace's
// unwrap/expect/panic denies apply; here an `expect` IS the assertion.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use chiefd_api::docstore::{self, Config, DEFAULT_MAX_BODY_BYTES, DEFAULT_READ_POOL};

fn fresh_config(dir: &tempfile::TempDir) -> Config {
    Config {
        bind: "127.0.0.1:0".to_string(),
        db_path: dir.path().join("org.sqlite").display().to_string(),
        read_pool: DEFAULT_READ_POOL,
        max_body_bytes: DEFAULT_MAX_BODY_BYTES,
    }
}

/// One raw `GET /v1/docs/health` over the real socket → (status code, body).
/// `Connection: close` lets us read to EOF; a blocking std socket on a
/// `spawn_blocking` thread avoids needing tokio's io-util extension traits.
async fn get_health(addr: std::net::SocketAddr) -> (u16, String) {
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("set read timeout");
        stream
            .write_all(
                b"GET /v1/docs/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .expect("write request");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read response");
        let text = String::from_utf8(raw).expect("utf8 response");
        let (head, body) = text.split_once("\r\n\r\n").expect("headers then body");
        let status: u16 = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("status code on the response line");
        (status, body.to_string())
    })
    .await
    .expect("join blocking http client")
}

#[tokio::test]
async fn fresh_store_reaches_ok_health_after_startup_ensure_schema_with_no_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fresh_config(&dir);

    // Exactly what `chiefd run` does now: bind, then ensure the schema on the
    // bound surface BEFORE serving — no docstore write anywhere in this test.
    let bound = docstore::bind(&config).await.expect("bind");
    let addr = bound.local_addr().expect("bound addr");
    bound.ensure_schema().await.expect("ensure schema at startup");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let _ = docstore::serve_bound(bound, async move {
            let _ = shutdown_rx.await;
        })
        .await;
    });

    let (status, body) = get_health(addr).await;
    assert_eq!(status, 200, "fresh store must be healthy after startup ensure_schema; body={body}");
    assert!(body.contains("\"status\":\"ok\""), "health body must report ok; body={body}");

    let _ = shutdown_tx.send(());
    let _ = server.await;
}

#[tokio::test]
async fn without_startup_ensure_schema_a_fresh_store_deadlocks_the_boot_gate() {
    // The pre-#390 state, reproduced: bind and serve WITHOUT ensuring the
    // schema. The launcher's `200 ok` boot gate can never pass here — this is
    // the exact 503 that SIGKILLed a healthy daemon at timeout.
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fresh_config(&dir);

    let bound = docstore::bind(&config).await.expect("bind");
    let addr = bound.local_addr().expect("bound addr");
    // Deliberately NO `bound.ensure_schema()` here.

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let _ = docstore::serve_bound(bound, async move {
            let _ = shutdown_rx.await;
        })
        .await;
    });

    let (status, body) = get_health(addr).await;
    assert_eq!(status, 503, "an un-ensured fresh store must report unhealthy; body={body}");
    assert!(
        body.contains("schema-missing"),
        "the 503 cause must name the missing schema; body={body}"
    );

    let _ = shutdown_tx.send(());
    let _ = server.await;
}

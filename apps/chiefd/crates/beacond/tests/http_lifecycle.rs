//! Integration test 13: the full company lifecycle over REAL HTTP against a
//! `127.0.0.1:0` bound socket — create -> create again (same directory,
//! idempotent) -> lookup(no location) -> register -> lookup(located) ->
//! heartbeat -> deregister -> lookup(still found, no location) -> delete ->
//! lookup(not found). Then the converge case: create, re-issue the
//! identical create, and assert `list` returns exactly one row — the
//! HTTP-level proof that re-running the claim after a crash is safe.
//!
//! Every company is named by its DIRECTORY. A slug rides along as a display
//! word and is never a key, which is why the converge company below can carry
//! the same one as its neighbour.
//!
//! Every fallible call threads through `?` rather than `.expect()`/
//! `.unwrap()`: the workspace lints deny both outside `#[cfg(test)]` scope,
//! and clippy's "allow in tests" carve-out only recognizes code lexically
//! inside a `#[test]`/`#[cfg(test)]` item — not a plain helper function
//! alongside one in a `tests/*.rs` integration file, which has neither.

use std::error::Error;
use std::sync::Arc;

use beacond::router::router;
use beacond::store::Registry;
use http_body_util::BodyExt;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn Error>>;

async fn spawn() -> Result<(tempfile::TempDir, String), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("beacond.sqlite");
    let registry = Arc::new(Registry::open(&path)?);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    // The real facts of THIS test process, exactly as beacond measures its own
    // at start — the surface `chief` reads to decide whether the running
    // beacond is the installed build.
    let app = router(registry, beacond::router::BeacondFacts::measure(path.display().to_string()));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((dir, format!("http://{addr}")))
}

async fn call(
    client: &Client<HttpConnector, axum::body::Body>,
    method: &str,
    url: &str,
    body: Option<Value>,
) -> Result<(u16, Value), Box<dyn Error>> {
    let request = if let Some(body) = body {
        hyper::Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body)?))?
    } else {
        hyper::Request::builder().method(method).uri(url).body(axum::body::Body::empty())?
    };
    let response = client.request(request).await?;
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await?.to_bytes();
    let json = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes)? };
    Ok((status, json))
}

/// The directory the company under test occupies, and the key its caller
/// minted for it (`sha256(dir)[..12]`, computed client-side).
const DIR: &str = "/work/northstar";
const KEY: &str = "0123456789ab";

#[tokio::test]
async fn the_full_lifecycle_over_real_http() -> TestResult {
    let (_dir, base) = spawn().await?;
    let client: Client<_, axum::body::Body> = Client::builder(TokioExecutor::new()).build_http();

    // create
    let (status, body) = call(
        &client,
        "POST",
        &format!("{base}/v1/company/create"),
        Some(json!({"dir": DIR, "key": KEY, "slug": "northstar"})),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(body["created"], json!(true));
    assert_eq!(body["company"]["dir"], json!(DIR));
    assert_eq!(
        body["company"]["key"],
        json!(KEY),
        "the key is served, never recomputed by a client"
    );

    // create again, same directory -> 200 created:false
    let (status, body) = call(
        &client,
        "POST",
        &format!("{base}/v1/company/create"),
        Some(json!({"dir": DIR, "key": KEY, "slug": "northstar"})),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(body["created"], json!(false));

    // lookup: no location
    let (status, body) = call(&client, "GET", &format!("{base}/v1/lookup?dir={DIR}"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(body["found"], json!(true));
    assert!(!body["company"].as_object().ok_or("company must be an object")?.contains_key("pid"));

    // register
    let (status, body) = call(
        &client,
        "POST",
        &format!("{base}/v1/register"),
        Some(
            json!({"dir": DIR, "url": "http://127.0.0.1:8794", "port": 8794, "pid": std::process::id(), "hostname": "box"}),
        ),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(body["registered"], json!(true));

    // lookup: located
    let (status, body) = call(&client, "GET", &format!("{base}/v1/lookup?dir={DIR}"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(body["company"]["pid"], json!(std::process::id()));

    // heartbeat
    let (status, body) = call(
        &client,
        "POST",
        &format!("{base}/v1/heartbeat"),
        Some(json!({"dir": DIR, "pid": std::process::id()})),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], json!(true));

    // deregister
    let (status, body) = call(
        &client,
        "POST",
        &format!("{base}/v1/deregister"),
        Some(json!({"dir": DIR, "pid": std::process::id()})),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(body["deregistered"], json!(true));

    // lookup: still found, no location
    let (status, body) = call(&client, "GET", &format!("{base}/v1/lookup?dir={DIR}"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(body["found"], json!(true));
    assert!(!body["company"].as_object().ok_or("company must be an object")?.contains_key("pid"));

    // list: STILL THERE. Deregistering is what `chief stop` does, and a
    // stopped company is a real state — it keeps its row, it keeps its data,
    // and `chief ls` must go on listing it. This is the assertion that stops
    // a removal fix from being written as "delete the row when the daemon goes
    // away", which would delete a company every time it was stopped.
    let (status, body) = call(&client, "GET", &format!("{base}/v1/list"), None).await?;
    assert_eq!(status, 200);
    let listed = body["companies"].as_array().ok_or("companies must be an array")?;
    assert!(
        listed.iter().any(|company| company["dir"] == json!(DIR)),
        "a deregistered company must still be discoverable: {listed:?}"
    );

    // delete
    let (status, body) =
        call(&client, "POST", &format!("{base}/v1/company/delete"), Some(json!({"dir": DIR})))
            .await?;
    assert_eq!(status, 200);
    assert_eq!(body["deleted"], json!(true));

    // lookup: not found
    let (status, body) = call(&client, "GET", &format!("{base}/v1/lookup?dir={DIR}"), None).await?;
    assert_eq!(status, 200);
    assert_eq!(body["found"], json!(false));

    // list: GONE. A removed company leaves discovery entirely — the row does
    // not survive as a ghost that `chief ls` goes on reporting for ever.
    let (status, body) = call(&client, "GET", &format!("{base}/v1/list"), None).await?;
    assert_eq!(status, 200);
    let listed = body["companies"].as_array().ok_or("companies must be an array")?;
    assert!(
        !listed.iter().any(|company| company["dir"] == json!(DIR)),
        "a removed company must be gone from discovery: {listed:?}"
    );

    // ---- converge case: re-running the claim after "a crash" is safe ----
    // It carries the SAME SLUG the deleted company had, from a different
    // directory. Under the slug-keyed registry that was a `409 slug-taken`;
    // here it is simply another company.
    let converge =
        json!({"dir": "/elsewhere/northstar", "key": "cafebabe0011", "slug": "northstar"});
    let (status, _) =
        call(&client, "POST", &format!("{base}/v1/company/create"), Some(converge.clone())).await?;
    assert_eq!(status, 200);

    let (status, _) =
        call(&client, "POST", &format!("{base}/v1/company/create"), Some(converge)).await?;
    assert_eq!(status, 200);

    let (status, body) = call(&client, "GET", &format!("{base}/v1/list"), None).await?;
    assert_eq!(status, 200);
    let companies = body["companies"].as_array().ok_or("companies must be an array")?;
    assert_eq!(companies.len(), 1, "re-issuing the identical create must not duplicate the row");
    assert_eq!(companies[0]["dir"], json!("/elsewhere/northstar"));
    assert_eq!(companies[0]["slug"], json!("northstar"));

    Ok(())
}

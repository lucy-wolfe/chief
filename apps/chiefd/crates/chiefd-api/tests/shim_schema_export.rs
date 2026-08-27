//! The chiefing client's copy of the request schemas is *derived*, never
//! transcribed.
//!
//! Plan §3.6 splits the tool surface by clock: the **shape** of every request
//! comes from the Rust structs via `schemars`, while the **prose** lives in a
//! separately versioned description catalog the shim merges at registration.
//! Neither the shim nor `@chief/chiefing` can call `schemars` (both are
//! TypeScript), so both read a checked-in JSON export. As of the E1/E2 move,
//! `packages/chiefing/generated/chiefd-request-schemas.json` is the ONE
//! drift-guarded copy this test maintains (D0 of #760/E1-S3): the shim
//! package's own frozen copy (see `shim/README.md`) remains in the tree,
//! unguarded, only because the shim still reads it at runtime while it is
//! the live extension client — it is deleted with the shim, unmaintained,
//! never synced against the chiefing copy by this or any other test.
//!
//! A checked-in file that nothing verifies is a second source of truth, and a
//! second source of truth is how the catalog silently drifts from the wire
//! types it is supposed to describe. So this test *is* the generator:
//!
//! * by default it compares the checked-in JSON to `wire::request_schemas()`
//!   and fails with the exact command to regenerate;
//! * with `UPDATE_SHIM_SCHEMAS=1` it rewrites the file. (The env var name is
//!   kept as-is post-move — renaming it would touch every doc and muscle
//!   memory for zero benefit.)
//!
//! The comparison is on parsed JSON, not bytes, so formatting choices in the
//! file can never fail CI for the wrong reason.

// Lint note: `clippy.toml`'s `allow-*-in-tests` switches only apply where
// `cfg(test)` is set, which integration tests in `tests/` are not. An `expect`
// in a test IS the assertion, so it is allowed here explicitly rather than by
// weakening the workspace lint. The `disallowed_methods` allow is the same
// deal: this test writes exactly one generated artifact, under an env guard,
// and is not the store or host layer the seam rule protects.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use chiefd_api::wire;
use serde_json::{Map, Value};

/// The canonical regeneration command, embedded in every failure message and
/// the exported `$comment` so a stale-export failure is self-documenting.
const REGEN_COMMAND: &str = "UPDATE_SHIM_SCHEMAS=1 cargo test --manifest-path \
     apps/chiefd/Cargo.toml -p chiefd-api --test shim_schema_export";

/// `packages/chiefing/generated/chiefd-request-schemas.json`, resolved from
/// this crate — the ONE drift-guarded copy (D0 of #760/E1-S3).
fn generated_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is baked in at compile time (#1002): under a
    // shared, persistent `CARGO_TARGET_DIR` a cached binary can outlive the
    // checkout it was built from. Fail loudly and specifically rather than
    // as a bare "file not found" from whatever reads this path next.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert!(
        manifest.is_dir(),
        "this test binary was compiled with CARGO_MANIFEST_DIR={} baked in at compile time, \
         but that directory no longer exists on this host (#1002: a shared CARGO_TARGET_DIR \
         served a binary built from a since-deleted checkout). Fix: `cargo clean -p chiefd-api` \
         and rebuild from a live checkout.",
        manifest.display()
    );
    manifest.join("../../../../packages/chiefing/generated/chiefd-request-schemas.json")
}

/// The exact document `@chief/chiefing` (and, unguarded, the legacy shim)
/// reads: `{$comment, ops:{op -> schema}}`.
fn document() -> Value {
    let ops: Map<String, Value> = wire::request_schemas()
        .into_iter()
        .map(|entry| (entry.op.to_owned(), entry.schema))
        .collect();
    let mut root = Map::new();
    root.insert(
        "$comment".into(),
        Value::String(format!(
            "GENERATED from chiefd-api's schemars derivation. Do not edit: run `{REGEN_COMMAND}`."
        )),
    );
    root.insert("ops".into(), Value::Object(ops));
    Value::Object(root)
}

#[test]
fn the_shim_schema_export_matches_the_frozen_wire_types() {
    let expected = document();
    let path = generated_path();

    if std::env::var_os("UPDATE_SHIM_SCHEMAS").is_some() {
        let body =
            format!("{}\n", serde_json::to_string_pretty(&expected).expect("schemas serialize"));
        #[allow(clippy::disallowed_methods)]
        std::fs::write(&path, body).expect("write the generated schema export");
    }

    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{}: {error}. Regenerate with `{REGEN_COMMAND}`", path.display())
    });
    let actual: Value = serde_json::from_str(&raw).expect("the generated export is valid JSON");

    assert_eq!(
        actual.get("ops"),
        expected.get("ops"),
        "packages/chiefing/generated/chiefd-request-schemas.json is stale. The \
         chiefing client's tool catalog is checked against it, so a drifted \
         export means the catalog describes fields that no longer exist. \
         Regenerate with `{REGEN_COMMAND}` and review the diff as a \
         cross-track contract change (plan §9)."
    );

    // Named per-op assertion after the whole-map one, because "goal.set is
    // missing" is a better failure than a 3,000-line diff.
    let ops = actual.get("ops").and_then(Value::as_object).expect("the export has an ops map");
    for spec in wire::OPERATIONS {
        assert!(
            ops.contains_key(spec.op),
            "{} is a registered operation with no exported schema — the shim \
             could not describe it even if a tool routed to it",
            spec.op
        );
    }
    assert_eq!(
        ops.len(),
        wire::OPERATIONS.len(),
        "the export carries an operation the registry does not"
    );
}

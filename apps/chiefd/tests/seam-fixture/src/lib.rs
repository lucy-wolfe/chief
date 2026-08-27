//! Planted violations. Every function below MUST make clippy fail; the CI step
//! `Assert the clippy seam actually fires` runs
//! `cargo clippy --manifest-path tests/seam-fixture/Cargo.toml -- -D warnings`
//! and treats a zero exit as a failure.
//!
//! This is the lint equivalent of a smoke detector test button: the seam rules
//! in `clippy.toml` are only worth anything if we know they still fire.

/// A handler module writing a file directly, bypassing `HostExecutor` and its
/// host transaction. Must trip `clippy::disallowed_methods`.
pub fn plants_a_direct_file_write() {
    let _ = std::fs::write("/tmp/chiefd-seam-fixture", b"nope");
}

/// A handler opening its own SQLite connection instead of going through the
/// per-company writer actor. Must trip `clippy::disallowed_types`.
pub fn plants_a_direct_file_handle() {
    let _ = std::fs::OpenOptions::new();
}

/// The shape that shipped three fail-fast regressions. Must trip
/// `clippy::unwrap_used`.
pub fn plants_an_unwrap(value: Option<u32>) -> u32 {
    value.unwrap()
}

/// Must trip `clippy::expect_used`.
pub fn plants_an_expect(value: Option<u32>) -> u32 {
    value.expect("nope")
}

//! Bake the release version into the library so `/v1/docs/health` can report it.
//!
//! # Why the API library stamps a version too
//!
//! The daemon binary already bakes `CHIEF_VERSION` (see
//! `chiefd-daemon/build.rs`), but the health handler that reports it lives
//! HERE, in `chiefd-api`, which is compiled as its own crate. A crate reads
//! only its OWN `env!`, so the handler needs this crate to be stamped with the
//! same value — and because one `cargo build` runs every crate's build script
//! with the same `CHIEF_RELEASE_VERSION`, the version this library reports and
//! the version the daemon binary prints are the same number, not two that could
//! drift. A plain `cargo build` sets neither and both fall back to the shared
//! `[workspace.package]` version, so they still agree.
//!
//! `chief upgrade` reads this reported version off a running company's health
//! surface to decide which companies still run an older build; a client refuses
//! to drive a daemon whose major/minor differs from its own. Both need the
//! number to be the daemon's true release version, which is why it is a build
//! input here rather than `CARGO_PKG_VERSION` alone.
fn main() {
    println!("cargo:rerun-if-env-changed=CHIEF_RELEASE_VERSION");
    let stamped = std::env::var("CHIEF_RELEASE_VERSION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    // `CARGO_PKG_VERSION` is always set by cargo, so the last arm is
    // unreachable in practice. It is a value rather than a panic because this
    // workspace denies `expect` on a `Result`, and because a build script that
    // aborts the build over a diagnostic string is worse than one that stamps
    // an obviously-wrong version an operator can read back.
    let version = stamped
        .or_else(|| std::env::var("CARGO_PKG_VERSION").ok())
        .unwrap_or_else(|| "0.0.0-unstamped".to_owned());
    println!("cargo:rustc-env=CHIEF_VERSION={version}");
}

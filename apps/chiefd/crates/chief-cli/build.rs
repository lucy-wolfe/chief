//! Bake the release version into the binary.
//!
//! # One number, three places, and why it is a build input
//!
//! A release is a directory named by its version (`~/.chief/versions/<v>`), a
//! `manifest.json` that states it, and three binaries that print it. `chief
//! upgrade` compares what the installed binary SAYS against the latest release,
//! so if the directory name and `--version` could disagree, an upgrade that had
//! landed perfectly would report itself as never having happened and offer
//! itself again, for ever.
//!
//! `scripts/release-chiefd.ts` resolves the version once and passes it here as
//! `CHIEF_RELEASE_VERSION`, then names the install directory with the same
//! value. The release workflow sets it from the tag it is building. A plain
//! `cargo build` sets nothing and gets `CARGO_PKG_VERSION`, which is what a
//! developer should see: their binary is not a release.
//!
//! NOT a file written into the source tree, and not a commit that bumps a
//! number: a version bump that has to be committed is a version bump somebody
//! forgets, and this project cuts a release from every green commit on `main`.
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

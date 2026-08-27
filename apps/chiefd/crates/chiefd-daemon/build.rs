// This crate's workspace-wide `-W missing-docs` lint applies even to a
// build script (compiled as its own tiny crate root) — the module-level
// commentary in this file IS the documentation; the lint just doesn't
// recognize `//` line comments on a `fn main()` as satisfying it the way it
// would a `//!` inner doc comment on a normal library crate.
#![allow(missing_docs)]

// #862 (defect 1): embeds a build-time CONTENT HASH into the `chiefd` binary
// — sha256 over the bytes of every real dependency source file THIS build
// actually compiled, baked in as a compile-time env var
// (`CHIEFD_BUILD_SOURCE_HASH_HEX`, read via `env!` in `main.rs`).
//
// WHY THIS FIXES THE REAL DEFECT (`check_chiefd_binary_precondition`,
// #856, formerly in the now-removed e2e crate): that check used to compare
// the *binary FILE's own on-disk mtime* against a scan of `apps/chiefd/**`'s
// source mtimes. Three independent problems with that:
//   1. The scan root was too wide — it included the e2e-only test tree
//      itself, which was NOT a dependency of the `chiefd` bin target, so an
//      e2e-only edit advanced the "newest source" timestamp without the
//      binary ever needing to rebuild. `cargo build --release --bin chiefd` then
//      correctly no-ops, the binary's mtime never advances, and the
//      precondition reports STALE forever — a trap, not a tax: the exact
//      command the error message recommends cannot clear it.
//   2. Even scoped correctly, a FILE's mtime is not trustworthy evidence of
//      when it was actually built: `git checkout`, `tar`, `rsync`, and CI
//      artifact-restore all commonly reset mtimes to the operation's own
//      time, unrelated to actual content. A binary genuinely built from a
//      DIFFERENT tree (a stale artifact cache, a wrong branch) can carry an
//      mtime that looks perfectly fresh.
//   3. #979: replacing the mtime scan with a build-TIME(STAMP) fingerprint
//      (this file's original #862 fix) traded one clock-dependent defect
//      for another. CI builds the release binary in one job and consumes it
//      in another via `actions/upload-artifact`/`download-artifact`; the
//      consumer job's OWN `actions/checkout` runs strictly after the
//      producer job finished (`needs:`), so every source file the consumer
//      checks out carries a LATER wall-clock mtime than the producer job's
//      embedded build-time fingerprint, UNCONDITIONALLY — not a race, a
//      guaranteed ordering. A "was the binary built after the newest
//      source" ORDERING check false-positives as stale on every single CI
//      run, for a binary that is in fact exactly current. Verified
//      empirically: `git clone` sets a checked-out file's mtime to clone
//      time, never the commit's committer time (confirmed live: a fresh
//      clone's file mtime was the clone instant, six minutes after that
//      file's own `git log --format=%ct`).
//
// The fix for defect 3 is to stop comparing TIMESTAMPS across a job
// boundary at all and compare CONTENT instead: same commit -> byte-identical
// source files -> the identical hash, regardless of which job checked it
// out or when. This is an EQUALITY check, not an ordering one.
//
// This hash fixes all three: it is embedded INSIDE the binary at compile
// time, immune to any later filesystem operation, and comparing it for
// EQUALITY against a freshly-computed hash of the current checkout removes
// wall-clock ordering from the mechanism entirely.
//
// THE PART THAT IS NOT CARGO'S DEFAULT BEHAVIOR, VERIFIED EMPIRICALLY (not
// assumed): a build script with NO `rerun-if-changed` directives reruns only
// when cargo decides ITS OWN crate's files changed — NOT when a dependency
// crate changes, even though cargo correctly recompiles and relinks the
// final binary in that case. Confirmed by hand on a build host: editing
// `chiefd-core/src/lib.rs` alone triggered a real `Compiling chiefd-host` /
// `chiefd-api` / `chiefd` rebuild (2m00s, not a no-op), but the embedded
// fingerprint did NOT change until the `rerun-if-changed` set below was
// added — this build script never reran, so a dependency-only change would
// silently keep reporting the OLD build time forever, reproducing defect
// 1's exact "can never clear" shape one level down. So the
// `rerun-if-changed` set below is not optional polish; without it this
// fingerprint is worse than the mtime scan it replaces. Re-verified after
// adding it: the SAME `chiefd-core` edit now advances the fingerprint, and
// an e2e-only edit still does not (see #862's DECISIONS.md entry for both
// measurements).
//
// NO `.unwrap()`/`.expect()`/`panic!()` below (this workspace's `[lints]`
// apply to build scripts too, confirmed the hard way — clippy denies all
// three crate-wide): every fallible step returns `Result<_, String>` and
// `main` reports a failure by printing to stderr and exiting non-zero,
// which cargo already surfaces as a build failure with that message shown
// — the same user-visible outcome as a panic, without the banned calls.
fn main() {
    if let Err(message) = run() {
        eprintln!("cargo:warning=chiefd build.rs failed: {message}");
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    stamp_release_version();
    let dirs = dependency_source_dirs()?;
    let lock_path = workspace_root()?.join("Cargo.lock");
    let hash_hex = hash_dependency_sources(&dirs, &lock_path)?;
    println!("cargo:rustc-env=CHIEFD_BUILD_SOURCE_HASH_HEX={hash_hex}");

    for dir in &dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    // The resolved dependency graph is itself an input: a `Cargo.lock`
    // update (a version bump) can change what actually got compiled in,
    // with zero local source line changed.
    println!("cargo:rerun-if-changed={}", lock_path.display());
    Ok(())
}

/// Bake the release version into the binary as `CHIEF_VERSION`.
///
/// A release is a directory named by its version (`~/.chief/versions/<v>`), a
/// `manifest.json` that states it, and three binaries that print it. `chief
/// upgrade` compares what the installed binary SAYS against the latest release,
/// so if the directory name and `--version` could disagree, an upgrade that had
/// landed perfectly would report itself as never having happened and offer
/// itself again, for ever. `scripts/release-chiefd.ts` resolves the version
/// once, passes it here as `CHIEF_RELEASE_VERSION`, and names the install
/// directory with the same value. A plain `cargo build` sets nothing and gets
/// `CARGO_PKG_VERSION` — the right thing for a developer, whose binary is not a
/// release. `CARGO_PKG_VERSION` is always set by cargo, so the last arm is
/// unreachable in practice; it is a value rather than a panic because this
/// workspace denies `expect` on a `Result` (see the module note above), and a
/// build script that aborts over a diagnostic string is worse than one that
/// stamps an obviously-wrong version an operator can read back.
fn stamp_release_version() {
    println!("cargo:rerun-if-env-changed=CHIEF_RELEASE_VERSION");
    let stamped = std::env::var("CHIEF_RELEASE_VERSION")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let version = stamped
        .or_else(|| std::env::var("CARGO_PKG_VERSION").ok())
        .unwrap_or_else(|| "0.0.0-unstamped".to_owned());
    println!("cargo:rustc-env=CHIEF_VERSION={version}");
}

/// `apps/chiefd` — the workspace root, two directories up from this crate's
/// own manifest directory (`CARGO_MANIFEST_DIR`, always set for build
/// scripts).
fn workspace_root() -> Result<std::path::PathBuf, String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("CARGO_MANIFEST_DIR is always set for build scripts: {error}"))?;
    Ok(std::path::Path::new(&manifest_dir).join("..").join(".."))
}

/// The real, transitive, WORKSPACE-MEMBER dependency closure of the
/// `chiefd` bin target, one source directory per crate — via `cargo
/// metadata`, never a hand-maintained path list (#862): a crate list is a
/// citation, correct today and silently wrong the moment someone adds a
/// crate to the bin's dependency graph, in the QUIET direction
/// (under-scoping `rerun-if-changed`, so a real dependency change stops
/// triggering a rebuild — the exact failure mode this whole file exists to
/// close).
fn dependency_source_dirs() -> Result<Vec<std::path::PathBuf>, String> {
    let workspace_manifest = workspace_root()?.join("Cargo.toml");
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(&workspace_manifest)
        .output()
        .map_err(|error| format!("failed to run `cargo metadata`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("`cargo metadata` produced invalid JSON: {error}"))?;
    resolve_dependency_dirs(&metadata, "chiefd-daemon")
}

/// Shared resolution logic: given `cargo metadata --format-version 1`'s
/// parsed output and a root package NAME, returns the manifest directory of
/// every WORKSPACE-MEMBER package reachable from that root's dependency
/// graph (transitively), including the root itself. External (crates.io)
/// dependencies are deliberately excluded — this is a lightweight,
/// dependency-free (no `cargo_metadata` crate) reimplementation of exactly
/// the fields needed: `packages[].{id,name,manifest_path}`,
/// `resolve.nodes[].{id,dependencies}`, `workspace_members`.
fn resolve_dependency_dirs(
    metadata: &serde_json::Value,
    root_package_name: &str,
) -> Result<Vec<std::path::PathBuf>, String> {
    let packages = metadata["packages"].as_array().ok_or("cargo metadata: packages[] missing")?;
    let mut manifest_dir_by_id = std::collections::HashMap::new();
    for package in packages {
        let id = package["id"].as_str().ok_or("package.id missing")?;
        let manifest_path =
            package["manifest_path"].as_str().ok_or("package.manifest_path missing")?;
        let dir = std::path::Path::new(manifest_path)
            .parent()
            .ok_or("manifest_path has no parent")?
            .to_path_buf();
        manifest_dir_by_id.insert(id.to_owned(), dir);
    }

    let root_id = packages
        .iter()
        .find(|package| package["name"] == root_package_name)
        .and_then(|package| package["id"].as_str())
        .ok_or_else(|| format!("cargo metadata: no package named \"{root_package_name}\" found"))?
        .to_owned();

    let nodes =
        metadata["resolve"]["nodes"].as_array().ok_or("cargo metadata: resolve.nodes[] missing")?;
    let node_by_id: std::collections::HashMap<&str, &serde_json::Value> = nodes
        .iter()
        .map(|node| Ok::<_, String>((node["id"].as_str().ok_or("node.id missing")?, node)))
        .collect::<Result<_, _>>()?;

    let workspace_members: std::collections::HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata: workspace_members[] missing")?
        .iter()
        .map(|id| id.as_str().ok_or("workspace_members[] entry is not a string"))
        .collect::<Result<_, _>>()?;

    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![root_id];
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(node) = node_by_id.get(id.as_str()) else { continue };
        let Some(deps) = node["dependencies"].as_array() else { continue };
        for dep in deps {
            if let Some(dep_id) = dep.as_str() {
                stack.push(dep_id.to_owned());
            }
        }
    }

    let mut dirs: Vec<std::path::PathBuf> = visited
        .into_iter()
        .filter(|id| workspace_members.contains(id.as_str()))
        .filter_map(|id| manifest_dir_by_id.get(&id).cloned())
        .collect();
    dirs.sort();

    // The graph walk above can never itself produce an empty result: the
    // root package is always visited first, and a workspace's own root
    // package is always a member of `workspace_members`. So a non-empty
    // `dirs` is guaranteed EXACTLY WHEN `packages[].id` and
    // `workspace_members[]` share the same id format — the `.filter(...)`
    // above matches them by string equality. `cargo metadata`'s package-id
    // format has changed across cargo releases; if it diverges again, this
    // filter silently yields zero, `run()` emits no `rerun-if-changed` lines
    // at all, and the embedded fingerprint freezes at whatever it already
    // was — restoring, silently, the exact "can never clear" defect this
    // whole file exists to close (see the module doc above). An empty
    // result here is never a legitimate answer; it is always this id-format
    // mismatch, so it is reported as the specific, actionable error it is
    // rather than left to manifest later as a fingerprint that mysteriously
    // stopped moving.
    if dirs.is_empty() {
        return Err(format!(
            "cargo metadata: resolved zero workspace dependency dirs for \"{root_package_name}\" — \
             packages[].id and workspace_members[] no longer share an id format, so \
             rerun-if-changed would be emitted for nothing and the build fingerprint would \
             freeze silently"
        ));
    }
    Ok(dirs)
}

/// True for exactly the files this build's content hash covers: `.rs`
/// sources, and `Cargo.toml`/`Cargo.lock` (a dependency version bump is a
/// real input with zero local source line changed). Skips `target/` the
/// same way `dependency_source_dirs`'s `rerun-if-changed` set does — it is
/// never a real input. Kept as its own named predicate (not inlined) so a
/// verify-side consumer of this hash can select EXACTLY this same set.
fn is_hashed_source_input(path: &std::path::Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("target") {
        return false;
    }
    path.extension().and_then(|ext| ext.to_str()) == Some("rs")
        || path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")
        || path.file_name().and_then(|name| name.to_str()) == Some("Cargo.lock")
}

/// sha256 over every hashed source input under `dirs`, plus `lock_path`,
/// in SORTED PATH order — deterministic regardless of directory-walk order
/// or which OS/filesystem produced it. Hashing PATH alongside BYTES (not
/// bytes alone) so a file rename with byte-identical content still changes
/// the hash: a rename is a real change to what the binary was built from,
/// even when no line inside any file moved.
fn hash_dependency_sources(
    dirs: &[std::path::PathBuf],
    lock_path: &std::path::Path,
) -> Result<String, String> {
    use sha2::Digest;

    // The REPO ROOT, not the workspace root two levels down (`apps/chiefd`):
    // `dirs` can include sibling crates outside `apps/chiefd` in principle,
    // and the repo root is the one ancestor every hashed path is guaranteed
    // to share. Paths are hashed RELATIVE to it (never absolute) so two
    // different checkouts of the identical commit -- different clone
    // directories, different CI runners, different job workspaces -- hash
    // identically. Hashing an ABSOLUTE path here would make the whole
    // mechanism checkout-location-dependent, reproducing the #979 defect
    // one layer down instead of fixing it.
    // MUST canonicalize before taking `.parent()` twice. `workspace_root()`
    // is built by literally APPENDING `../..` to `CARGO_MANIFEST_DIR`
    // (`Path::join`, never resolved) — `Path::parent()` operates on the
    // syntactic component list, so on an un-normalized path it strips the
    // literal trailing `..` components themselves rather than walking back
    // up real directories. Verified empirically: this bug, uncaught,
    // resolved `repo_root` to `apps/chiefd/crates/chiefd-daemon` (the crate's OWN
    // directory) instead of the actual repo root, and every hashed path
    // outside that one crate then failed its `strip_prefix` and silently
    // fell back to an ABSOLUTE path — internally consistent within one
    // checkout, but different between any two checkout locations, which is
    // exactly the checkout-dependence this whole mechanism exists to
    // remove. Caught by the two-checkout topology test comparing a real
    // built binary's embedded hash against a second, separately-hashed
    // clone — a same-checkout test could not have found this, since the
    // wrong repo_root is still self-consistent when only one checkout ever
    // exists.
    let repo_root = std::fs::canonicalize(workspace_root()?)
        .map_err(|error| format!("cannot canonicalize workspace root: {error}"))?
        .parent()
        .and_then(|p| p.parent())
        .ok_or("workspace_root() has no repo-root ancestor two levels up")?
        .to_path_buf();

    // `lock_path` (and `dirs`, from `cargo metadata`'s own `manifest_path`
    // values) are already canonical in practice, but `lock_path` here comes
    // from `workspace_root()?.join("Cargo.lock")` — `workspace_root()` is
    // the SAME un-normalized `CARGO_MANIFEST_DIR/../..` path that caused
    // the `repo_root` bug above. Canonicalizing it independently (not
    // derived from the already-fixed `repo_root`, so a future change to one
    // cannot silently desync from the other) guarantees `strip_prefix`
    // below produces the SAME relative-path spelling a verify-side
    // consumer of this hash would produce for the identical file — the
    // exact mismatch a canonical `repo_root` alone did not catch, because
    // `strip_prefix` succeeds syntactically
    // against an un-normalized suffix too, just with the literal `..`
    // segments left inside the "relative" result.
    let lock_path = std::fs::canonicalize(lock_path)
        .map_err(|error| format!("cannot canonicalize {}: {error}", lock_path.display()))?;

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = dirs.to_vec();
    stack.push(lock_path);
    while let Some(path) = stack.pop() {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot stat {}: {error}", path.display())),
        };
        if metadata.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("target") {
                continue;
            }
            let entries = std::fs::read_dir(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    format!("cannot read an entry under {}: {error}", path.display())
                })?;
                stack.push(entry.path());
            }
            continue;
        }
        if is_hashed_source_input(&path) {
            files.push(path);
        }
    }
    files.sort();

    let mut hasher = sha2::Sha256::new();
    for file in &files {
        let relative = file.strip_prefix(&repo_root).unwrap_or(file);
        let bytes = std::fs::read(file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0u8]); // separator: a path/content boundary a naive concatenation could blur
        hasher.update(&bytes);
    }
    // sha2 0.11 returns a `hybrid_array::Array`, which does not implement
    // `LowerHex`. A build script cannot import `chiefd_core::hexdigest`
    // (that crate is not a build-dependency of the crate it builds), so the
    // same lower-case, zero-padded encoding is written out here.
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

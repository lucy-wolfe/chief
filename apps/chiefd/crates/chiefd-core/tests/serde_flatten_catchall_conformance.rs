// An integration test is its own crate, so `clippy.toml`'s
// `allow-expect-in-tests` only reaches `#[test]` functions; the scaffolding
// helpers below expect/panic by construction — a failure here is the test
// failing, which is the intended outcome.
#![allow(clippy::expect_used, clippy::panic)]

//! Gate (#844): every `#[serde(flatten)] extra: BTreeMap<..>` catch-all under
//! `chiefd-core/src/store/` must be paired with a write-time
//! "reject any non-empty `extra`" guard (item D, `unmodeled-keys`) somewhere
//! in the same module — or be on the explicit, justified allowlist below.
//!
//! # Why this exists
//!
//! #844 (spot-audit during the E2-S2/#771 merge) found that `packages/chiefing`
//! models none of the 25+ `extra` catch-alls chiefd's store layer carries. That
//! omission turned out to be safe FOR EVERY CURRENT MODULE: 25 of 26 reject any
//! non-empty `extra` on write (422 `unmodeled-keys`) and never populate it on
//! read, so a TypeScript type that never declares `extra` cannot lose data —
//! there is nothing on the wire to lose. The one exception,
//! `organization.rs`'s `OrganizationManifest`/`DepartmentRecord`/`PersonRecord`,
//! is a deliberate, in-file-documented exception for the hand-editable
//! manifest JSON blob (consumed by untyped `apps/cli/src/legacy` code, never
//! by `packages/chiefing`).
//!
//! That safety property was never mechanically enforced, though — it held
//! only because every module's author happened to copy the item-D pattern.
//! This test makes it structural: a NEW store that adds a flatten catch-all
//! without wiring the write-time guard fails CI immediately, instead of
//! silently reopening the exact risk #844 raised. It also re-proves the
//! CURRENT closed list on every run, so a regression that silently removes an
//! existing guard is caught too.
//!
//! # Scope
//!
//! Only `chiefd-core/src/store/` — the store layer's `extra` catch-all
//! pattern. `chiefd-api/src/wire/company.rs`'s two `#[serde(flatten)]` uses
//! are a DIFFERENT pattern (tagged-enum variant composition, not an "any
//! unmodeled key" catch-all) and that whole `CompanyRemove*` protocol is
//! slated for deletion (E7-S4/S7, D23/F17) — out of scope by construction.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Walk up from this crate's manifest dir to the repository root — the first
/// ancestor that actually contains `apps/chiefd`. Mirrors `port_provenance.rs`.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `CARGO_MANIFEST_DIR` is baked in at compile time (#1002): under a
    // shared, persistent `CARGO_TARGET_DIR` a cached binary can outlive the
    // checkout it was built from, so EVERY ancestor of the baked path can be
    // simultaneously absent from disk and the `.find` below fails as a
    // generic `Option::expect` with no clue why. Name the real cause first.
    assert!(
        manifest.is_dir(),
        "this test binary was compiled with CARGO_MANIFEST_DIR={} baked in at compile time, \
         but that directory no longer exists on this host (#1002: a shared CARGO_TARGET_DIR \
         served a binary built from a since-deleted checkout). Fix: `cargo clean -p chiefd-core` \
         and rebuild from a live checkout.",
        manifest.display()
    );
    manifest
        .ancestors()
        .find(|dir| dir.join("apps/chiefd").is_dir())
        .expect("a repo root containing apps/chiefd is an ancestor of this crate")
        .to_path_buf()
}

/// Collect every `.rs` file under `dir`, skipping any `target` build directory
/// and any `tests`/`tests.rs` unit-test module (a test asserting the refusal
/// does not itself need to BE the refusal).
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// A module whose `#[serde(flatten)] extra` catch-all is deliberately
/// PERMISSIVE (not write-time-rejected), with the citation proving the
/// exception is real and still documented — not a stale allowlist entry.
struct AllowedExtra {
    /// Repo-relative path of the struct-defining file.
    path: &'static str,
    /// A substring that must still be present in that file, proving the
    /// justification this allowlist entry relies on has not silently
    /// disappeared (the bidirectional half — a stale entry fails too).
    justification_substring: &'static str,
}

const ALLOWED_PERMISSIVE_EXTRA: &[AllowedExtra] = &[AllowedExtra {
    path: "apps/chiefd/crates/chiefd-core/src/store/organization.rs",
    // The in-file doc block explaining why the hand-edited manifest JSON blob
    // keeps unknown keys instead of rejecting them (operator safety).
    justification_substring: "Unknown fields round-trip (operator safety)",
}];

#[test]
fn every_serde_flatten_extra_catchall_has_write_time_unmodeled_key_enforcement() {
    let root = repo_root();
    let store_dir = root.join("apps/chiefd/crates/chiefd-core/src/store");
    assert!(store_dir.is_dir(), "expected {} to exist", store_dir.display());

    let mut files = Vec::new();
    rust_sources(&store_dir, &mut files);
    files.sort();
    assert!(!files.is_empty(), "found no Rust sources under chiefd-core/src/store");

    // `#[serde(flatten)]` directly above a field literally named `extra` — the
    // catch-all shape, not any other structural use of `flatten`.
    let catchall =
        Regex::new(r"#\[serde\(flatten\)\]\s*\n\s*pub extra:\s*BTreeMap").expect("valid regex");
    // Either spelling — the shared `UNMODELED_KEYS` constant, or a module that
    // inlines the literal string (
    let enforcement = Regex::new(r"(?i)UNMODELED_KEYS|unmodeled-keys").expect("valid regex");

    let mut catchall_files: Vec<PathBuf> = Vec::new();
    for file in &files {
        let content = fs::read_to_string(file).unwrap_or_default();
        if catchall.is_match(&content) {
            catchall_files.push(file.clone());
        }
    }
    catchall_files.sort();

    // A floor, not a ceiling (#844 counted 25; a 26th had landed by the time
    // of this audit) — this must never silently drop to near-zero because a
    // path/regex changed shape and stopped matching anything.
    //
    // Lowered 19 -> 17 again when two more row modules were DELETED with the
    // retired messaging channel they fronted: two genuine members of the
    // population, gone with their subsystem, so the floor tracks the shrink
    // for the same reason.
    //
    // Lowered 17 -> 13 when the four subsystems this change deletes took their
    // row modules with them: `acks_rows.rs` (the acknowledgement-receipt
    // queue), and the task, memory and learned-skill row modules. Four genuine
    // members of the population, gone with their subsystems.
    //
    // Lowered 20 -> 19 when `supervisor_process_state_rows.rs` and
    // `supervisor_armed_intent_rows.rs` were DELETED: both fronted the detached
    // org-supervisor's process state, whose writer #825 retired. The population
    // genuinely shrank by two, so the floor tracks it. This is the one legal
    // reason to move this number — a file matching and then not matching is
    // the failure it exists to catch, and stays a failure.
    // Lowered 13 -> 12 when `goal_intents_rows.rs` was DELETED with the goal
    // feature — the same one legal reason as the two above.
    // Lowered 12 -> 11 when `boot_lease_rows.rs` was DELETED with the
    // daemon-side CEO boot (chief-home-is-cwd §4c): the lease it fronted has no
    // writer left, so the module is a genuine member of the population that
    // went with its subsystem.
    // Lowered 11 -> 10 when `materialization_rows.rs` was deleted with the
    // durable checkpoint subsystem (chief-home-is-cwd §4d).
    assert!(
        catchall_files.len() >= 10,
        "expected at least 10 serde-flatten extra catch-alls under store/, found {}: {:?}",
        catchall_files.len(),
        catchall_files,
    );

    let mut unenforced = Vec::new();
    for file in &catchall_files {
        let rel = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");

        if let Some(allowed) = ALLOWED_PERMISSIVE_EXTRA.iter().find(|a| a.path == rel) {
            let content = fs::read_to_string(file).unwrap_or_default();
            assert!(
                content.contains(allowed.justification_substring),
                "allowlist entry for {} is stale: its justification (\"{}\") is no longer present — \
                 either the exception was removed (drop the allowlist entry) or the doc moved \
                 (update `justification_substring`)",
                rel,
                allowed.justification_substring,
            );
            continue;
        }

        // Enforcement may live in the SAME file, or in a same-named companion
        // directory (`supervision.rs` declares the struct; `supervision/rows.rs`
        // enforces it). Check both.
        let mut companions = vec![file.clone()];
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned());
        if let (Some(parent), Some(stem)) = (file.parent(), stem) {
            let sibling_dir = parent.join(&stem);
            if sibling_dir.is_dir() {
                rust_sources(&sibling_dir, &mut companions);
            }
        }

        let found = companions.iter().any(|companion| {
            fs::read_to_string(companion).map(|c| enforcement.is_match(&c)).unwrap_or(false)
        });

        if !found {
            unenforced.push(rel);
        }
    }

    assert!(
        unenforced.is_empty(),
        "these store modules declare a `#[serde(flatten)] extra` catch-all with NO write-time \
         `unmodeled-keys` rejection anywhere in the module (checked the file itself and any \
         same-named companion directory) — either wire item D's reject-on-publish guard, or add a \
         justified, cited entry to ALLOWED_PERMISSIVE_EXTRA in this test (mirroring \
         `organization.rs`'s manifest exception) and explain why the omission is safe: {unenforced:#?}"
    );
}

/// The allowlist itself must name real files that actually carry the pattern
/// today — an entry for a file that no longer declares a permissive `extra`
/// (renamed, deleted, or tightened to reject like everything else) is exactly
/// the kind of stale exception this gate exists to prevent from going unnoticed.
#[test]
fn the_permissive_extra_allowlist_names_only_files_that_still_carry_a_flatten_extra_catchall() {
    let root = repo_root();
    let catchall =
        Regex::new(r"#\[serde\(flatten\)\]\s*\n\s*pub extra:\s*BTreeMap").expect("valid regex");
    for allowed in ALLOWED_PERMISSIVE_EXTRA {
        let path = root.join(allowed.path);
        let content = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("allowlist names {}, which cannot be read: {error}", allowed.path)
        });
        assert!(
            catchall.is_match(&content),
            "allowlist entry {} no longer declares a `#[serde(flatten)] extra` catch-all — \
             remove the stale entry",
            allowed.path,
        );
    }
}

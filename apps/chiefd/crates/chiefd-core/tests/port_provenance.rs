// An integration test is its own crate, so `clippy.toml`'s
// `allow-expect-in-tests` only reaches `#[test]` functions; the scaffolding
// helpers below expect/panic by construction — a failure here is the test
// failing, which is the intended outcome.
#![allow(clippy::expect_used, clippy::panic)]

//! Gate: every `Port of <path>` doc-comment in `apps/chiefd` must name a
//! source file that still exists in the repository.
//!
//! A store whose doc-comment declares it a port of a repo-relative source path
//! is making a checkable claim: that the TypeScript it mirrors is still there.
//! When the source module is deleted from `main` — as the provider-readiness
//! fence and the fleet-suppression latch were — the Rust port is dead code that
//! survives only because nothing re-checks the claim. This test is that re-check.
//!
//! Scope is deliberately narrow: only backtick-quoted tokens that begin with
//! `src/`, `apps/cli/src/legacy/`, or `packages/piing/src/` (an actual
//! repo-relative path, optionally carrying a `:line` citation suffix which is
//! stripped) are treated as provenance claims. Bare filename citations like an
//! inline function reference are not paths and are not checked here — they are
//! annotations, not the store's declared source.
//!
//! `apps/cli/src/legacy/` was added by E4-S1/#787's `git mv` of the repo-root
//! `src/` tree: every existing citation was repointed to it, and this
//! pattern was widened (not replaced) in the same change — the original
//! `src/`-only pattern silently stopped matching any of the 12 repointed
//! citations, which would have made this guard pass by no longer looking
//! rather than by the claims being checkable again. `src/` is kept
//! alongside `apps/cli/src/legacy/` because a future citation may validly
//! name a workspace package's own `src/`. `packages/piing/src/` is the public
//! runtime-authority source root after #785 removed the final legacy priority
//! source.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Walk up from this crate's manifest dir to the repository root — the first
/// ancestor that actually contains `apps/chiefd`.
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

/// Collect every `.rs` file under `dir`, skipping any `target` build directory.
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

fn provenance_path_token() -> Regex {
    Regex::new(r"`((?:src|apps/cli/src/legacy|packages/piing/src)/[^`:]+)").expect("valid regex")
}

#[test]
fn every_port_of_doc_comment_names_a_source_path_that_exists() {
    let root = repo_root();
    let chiefd = root.join("apps/chiefd");

    let mut files = Vec::new();
    rust_sources(&chiefd, &mut files);
    files.sort();
    assert!(!files.is_empty(), "found no Rust sources under apps/chiefd");

    // A provenance claim: `Port of` somewhere on the line, and a backtick-quoted
    // token that starts with `src/`, `apps/cli/src/legacy/` (the post-#787
    // home of the moved repo-root `src/` tree), or the public Piing runtime
    // root `packages/piing/src/`. `[^`:]+` stops at a `:line` citation suffix
    // and at the closing backtick, so
    // `apps/cli/src/legacy/organization/x.ts:12-20` yields the path
    // `apps/cli/src/legacy/organization/x.ts`.
    let path_token = provenance_path_token();

    let mut dead: Vec<String> = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("readable source file");
        for (lineno, line) in text.lines().enumerate() {
            if !line.contains("Port of") {
                continue;
            }
            for capture in path_token.captures_iter(line) {
                let claimed = &capture[1];
                if !root.join(claimed).exists() {
                    dead.push(format!(
                        "{}:{} declares `Port of {}` but that path does not exist",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        lineno + 1,
                        claimed
                    ));
                }
            }
        }
    }

    assert!(
        dead.is_empty(),
        "dead `Port of` provenance ({} claim(s)):\n{}",
        dead.len(),
        dead.join("\n")
    );
}

// === #853: no production path may be constructed against a retired root ===
//
// The #787 sweep found and fixed seven citing sites, one of which built
// `<launcher_root>/src/organization/
// org-learned-skill-extract.ts` after the move and failed open BY DESIGN —
// silently disabling learned-skill extraction on every deployment for the
// whole interval, with nothing failing and nothing lying, because a
// fail-open is invisible to every gate by construction. A sweep is a
// snapshot; nothing stops a fresh `.join("src")` next week. This is the
// durable form: a structural check, not another one-time enumeration.
//
// # The distinguishing signal
//
// Every retired subtree used to live directly under the repo-root `src/`
// (`src/organization`, `src/foundation`, …) and now lives under
// `apps/cli/src/legacy/` — literally the SAME six directory/file names,
// with `legacy` interposed. So the retired shape and the correct shape
// differ in exactly one place: what comes immediately after `src` in the
// constructed path.
//
//   RETIRED (flag):  …/src/organization/…        (org-name right after src)
//   CORRECT (allow): …/src/legacy/organization/…  (legacy right after src)
//
// That is precise on its own — it does not need a side channel distinguishing
// "was this inside a doc comment" from "was this executable", because a doc
// comment's `Port of` citation is ALSO always the retired shape
// (`src/organization/…`, never `src/legacy/organization/…` — #844/D26
// citations are frozen historical statements, they don't get rewritten to
// the new location). So doc comments must be excluded by construction
// (stripped as comments, same as `every_port_of_doc_comment_…` already
// scopes to `Port of` lines specifically) — the retired/correct signal alone
// cannot tell a citation from a construction.
//
// # Scope: comments and test code are prose, not production
//
// `production_only_text` strips every comment line (`//`, `///`, `//!` —
// this also removes every `Port of` doc-comment citation, which is the
// point: those are protected historical statements, not path construction)
// and truncates the file at its first `#[cfg(test)]`, mirroring the
// established convention (also used by #844's conformance test) that a test
// module is the last item in a production file. A `.rs` file that is ENTIRELY
// a test — anything under a `tests/` directory, or named `tests.rs` / ending
// `_tests.rs` / `_test.rs` — is skipped outright: a fixture proving an old
// path is correctly REJECTED, or a fake filesystem tree standing in for one,
// legitimately spells the retired shape without constructing a real one.

/// The top-level entries `apps/cli/src/legacy/` holds today — formerly
/// direct children of the repo-root `src/` (E4-S1/#787's `git mv`). Extend
/// this list, not the regex, the next time a tree moves: add the new
/// retired root's bare name (the segment that used to sit directly under
/// `src/`, and now sits under `src/legacy/` or wherever the new root is).
///
/// Two former entries are absent because they no longer exist ANYWHERE — the
/// operator-lifecycle and bare-command trees were ported to Rust and deleted
/// rather than moved, so there is no correct `legacy`-interposed shape for
/// this check to allow and nothing for it to distinguish.
const RETIRED_ROOTS: &[&str] = &["cli-help.ts", "cli.ts", "foundation", "organization"];

/// `true` for a whole file this check should not scan at all: an
/// integration-test crate file, or a file whose own job is exercising
/// path-construction logic (its name says so) rather than being it.
fn is_test_only_file(path: &Path) -> bool {
    if path.components().any(|c| c.as_os_str() == "tests") {
        return true;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name == "tests.rs" || name.ends_with("_tests.rs") || name.ends_with("_test.rs")
}

/// Strip prose (comment-only lines) and everything from the first
/// `#[cfg(test)]` onward, leaving only text that is actually compiled as
/// production code.
fn production_only_text(full: &str) -> String {
    let before_tests = full.find("#[cfg(test)]").map_or(full, |idx| &full[..idx]);
    before_tests
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find every construction of a path against a retired root in `text`
/// (already comment/test-stripped production source), returning a
/// human-readable description of each — `.join("src")` immediately chained
/// into a retired root name (skipping `legacy`), and the same shape spelled
/// as a single string literal (`"src/organization/…"`, `format!` bodies,
/// etc.). Empty when the text is clean.
fn find_retired_root_constructions(text: &str) -> Vec<String> {
    let escaped_roots =
        RETIRED_ROOTS.iter().map(|r| regex::escape(r)).collect::<Vec<_>>().join("|");

    // `.join("src")`, then — with only whitespace between, so a multi-line
    // rustfmt'd chain still matches — `.join("<retired root>")` directly.
    // The correct shape always has `.join("legacy")` in between, which this
    // pattern does not allow for, by construction.
    let join_chain =
        Regex::new(&format!(r#"\.join\(\s*"src"\s*\)\s*\.join\(\s*"({escaped_roots})"\s*\)"#))
            .expect("valid regex");
    // The same shape as a string literal: `src/<retired root>` as a
    // contiguous substring — not anchored to the opening quote, so a
    // `format!("{root}/src/foundation/…")`-style interpolation still matches.
    // Never matches the correct shape, which reads `src/legacy/<retired
    // root>` — `legacy` sits between the slash and the root name, so
    // `src/(root)` never appears as a contiguous substring there. Comments
    // are already stripped before this runs, so any surviving occurrence is
    // inside compiled code — in practice always a string literal, since bare
    // `src/organization` is not otherwise valid Rust syntax.
    let literal = Regex::new(&format!(r#"src/({escaped_roots})(/|"|$)"#)).expect("valid regex");

    let mut found = Vec::new();
    for capture in join_chain.captures_iter(text) {
        found.push(format!(
            ".join(\"src\").join(\"{}\") — retired root reconstructed without `legacy`",
            &capture[1]
        ));
    }
    for capture in literal.captures_iter(text) {
        found.push(format!(
            "string literal `src/{}` — retired root reconstructed without `legacy`",
            &capture[1]
        ));
    }
    found
}

#[test]
fn no_production_path_is_constructed_against_a_retired_root() {
    let root = repo_root();
    let chiefd = root.join("apps/chiefd");

    let mut files = Vec::new();
    rust_sources(&chiefd, &mut files);
    files.sort();
    assert!(!files.is_empty(), "found no Rust sources under apps/chiefd");

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        if is_test_only_file(file) {
            continue;
        }
        let text = fs::read_to_string(file).expect("readable source file");
        let production = production_only_text(&text);
        for finding in find_retired_root_constructions(&production) {
            offenders
                .push(format!("{}: {finding}", file.strip_prefix(&root).unwrap_or(file).display()));
        }
    }

    assert!(
        offenders.is_empty(),
        "production Rust code constructs a path against a retired root ({} site(s)) — the tree \
         moved under E4-S1/#787 and never repointed this construction, exactly the shape that \
         silently disabled learned-skill extraction until #787 fallout caught it:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

#[cfg(test)]
mod retired_root_fixture_tests {
    use super::{find_retired_root_constructions, production_only_text};

    /// The retired shape, as a join chain — the exact pattern a citing site
    /// had before the #787 fallout fix.
    #[test]
    fn a_retired_join_chain_is_flagged() {
        let source = r#"
fn resolve() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(launcher_root);
    root.join("src")
        .join("organization")
        .join("org-learned-skill-extract.ts")
}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("organization"), "{found:?}");
    }

    /// The retired shape as one string literal (a single-`join`, or a
    /// `format!`/`PathBuf::from` construction) is caught the same way.
    #[test]
    fn a_retired_string_literal_is_flagged() {
        let source = r#"
fn resolve(root: &str) -> String {
    format!("{root}/src/foundation/paths.ts")
}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("foundation"), "{found:?}");
    }

    /// The CORRECTED shape — `legacy` interposed, exactly what the citing
    /// site was repointed to — must never be flagged.
    #[test]
    fn the_corrected_legacy_interposed_shape_is_not_flagged() {
        let source = r#"
fn resolve() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(launcher_root);
    root.join("apps")
        .join("cli")
        .join("src")
        .join("legacy")
        .join("organization")
        .join("org-learned-skill-extract.ts")
}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert!(found.is_empty(), "{found:?}");
    }

    /// A `Port of` doc-comment citation — the D26-protected historical
    /// statement every `*_rows.rs` module carries — names the exact retired
    /// shape as prose. It must never be flagged: this is the whole reason
    /// the check can't be a bare grep for the retired shape.
    ///
    /// Deliberately spelled without the literal phrase "Port of" immediately
    /// beside a backtick-quoted path: this file is itself scanned by
    /// [`every_port_of_doc_comment_names_a_source_path_that_exists`], and a
    /// synthetic fixture that fully mimics that pattern would register as a
    /// (dead) claim there too. The property under test — a comment naming
    /// the retired shape must not trip THIS check — doesn't depend on
    /// matching that other check's exact phrasing.
    #[test]
    fn a_port_of_doc_comment_citation_is_not_flagged() {
        let source = r#"
//! This store's origin citation (D26, frozen historical statement): the
//! pre-#787 path `src/organization/org-durable-store.ts`.

fn unrelated() {}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert!(found.is_empty(), "{found:?}");
    }

    /// A regular (non-doc) comment mentioning the retired shape — an
    /// explanatory aside, not a citation with its own protocol — must also
    /// not be flagged: only comment-STRIPPED text is scanned at all.
    #[test]
    fn a_plain_comment_mentioning_the_retired_shape_is_not_flagged() {
        let source = r#"
// The old path was root.join("src").join("organization"), before #787.
fn unrelated() {}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert!(found.is_empty(), "{found:?}");
    }

    /// Code inside a `#[cfg(test)]` module — a fixture asserting the retired
    /// path is properly ABSENT/rejected, say — is test code, not production,
    /// and must not be flagged even though the raw text contains the shape.
    #[test]
    fn retired_shape_inside_a_cfg_test_module_is_not_flagged() {
        let source = r#"
fn production_code() {}

#[cfg(test)]
mod tests {
    #[test]
    fn old_path_fixture() {
        let p = std::path::PathBuf::from("root").join("src").join("organization");
        assert!(!p.exists());
    }
}
"#;
        let found = find_retired_root_constructions(&production_only_text(source));
        assert!(found.is_empty(), "{found:?}");
    }
}

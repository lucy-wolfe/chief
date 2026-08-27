// `clippy.toml`'s `allow-expect-in-tests` only reaches functions that carry
// `#[test]`, and an integration test is its own crate. The helpers below are
// test scaffolding by construction — a failed `expect` here is the test
// failing, which is the intended outcome.
#![allow(clippy::expect_used, clippy::panic)]

//! The ported sentinel grep test: the fence must be impossible to bypass
//! *silently* (plan §5.5, inv c-1; TESTING.md §3.2).
//!
//! # What the TS test asserted, and why this one is shaped differently
//!
//! The predecessor system threaded launch intent through as an optional value,
//! so it needed two deliberately-differently-spelled sentinel constants —
//! `"launch-intent:unfenced"` in `org-launch-intent.ts` and
//! `"unfenced-launch-intent:no-fence"` in `org-activity.ts` — plus a grep test
//! asserting each appears in exactly one source file. That test is still live
//! on the TS side (`tests/deploy-hardening.test.ts`, "no production source path
//! runs unfenced") and stays there for the migration window.
//!
//! It cannot be ported *literally*, because the Rust design deletes the thing
//! it guards: `LaunchIntent` has no permissive variant, so there is no
//! unfenced sentinel to confine. Porting the literal strings would create dead
//! constants and a test that guards nothing.
//!
//! What ports is the idea, generalized and strengthened. The TS test asked
//! "does anything outside this module name the escape hatch?". These ask:
//!
//! 1. **Nothing outside a store's own module can reach its rows at all.** The
//!    `documents` key is the only way to touch a store through the M4 ledger
//!    API, so confining the key confines the store: every other module must go
//!    through the typed, polarity-carrying accessors. A handler that wants to
//!    peek at the fence cannot do it without editing this test.
//! 2. **The permissive variant does not exist**, asserted against source text
//!    rather than trusted to review — this is the exact bug the plan calls
//!    "one refactor away from opening a 28-agent fleet".
//! 3. **No unfenced escape hatch has been reintroduced** under any of the
//!    spellings the TS system used.

use std::fs;
use std::path::{Path, PathBuf};

use chiefd_core::polarity::StoreKind;
use chiefd_core::store::launch_intent::LaunchIntentStore;
use chiefd_core::store::StoreId;

/// `apps/chiefd/crates` — every Rust source chiefd ships.
///
/// `CARGO_MANIFEST_DIR` is baked in at compile time (#1002): under a shared,
/// persistent `CARGO_TARGET_DIR` a cached binary can outlive the checkout it
/// was built from. Fail loudly and specifically rather than as a bare
/// "file not found" from whatever reads source text out of a dead directory.
fn crates_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        manifest.is_dir(),
        "this test binary was compiled with CARGO_MANIFEST_DIR={} baked in at compile time, \
         but that directory no longer exists on this host (#1002: a shared CARGO_TARGET_DIR \
         served a binary built from a since-deleted checkout). Fix: `cargo clean -p chiefd-core` \
         and rebuild from a live checkout.",
        manifest.display()
    );
    manifest.parent().map(Path::to_path_buf).expect("crates/chiefd-core has a parent")
}

/// Drop comment lines.
///
/// These guards are about what the *compiler* sees. Documentation has to be
/// able to say "there is no `Unfenced` variant" and to show the registry's
/// usage example; a guard that forbade naming the hazard would make it
/// undocumentable, and an undocumented hazard is how it comes back.
fn code_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `src/**/*.rs` under the workspace's crates, as (repo-relative path,
/// production text). Test modules and comments are cut off: a test is
/// *supposed* to name the things these guards confine, and so is a doc
/// comment.
fn production_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                // A `#[cfg(test)] mod tests;` in its own file is test code, and
                // is stripped for exactly the same reason the inline
                // `#[cfg(test)] mod tests { … }` form is below: a test may
                // legitimately name a store's key or construct a permissive
                // fence, and counting it as production would either force
                // every store's tests into `mod.rs` or make the guard cry wolf
                // until somebody weakened it. The *declaration* still lives in
                // the module file, which IS scanned, so a `mod tests;` that
                // was not `#[cfg(test)]`-gated would ship the file into the
                // binary and is caught by
                // `every_out_of_line_test_module_is_cfg_test_gated`.
                out.push(path);
            }
        }
    }

    let root = crates_root();
    // ONLY the crates that can actually perform the bypass this guard exists to
    // prevent — the ones that link `chiefd-core` and therefore have a store
    // layer to go around. The rule is a Cargo dependency edge, not a name list:
    // a crate that gains a `chiefd-core` dependency comes back into scope by
    // itself, and one that never had it was never able to offend.
    //
    // #751/P10 is why this is here. `chief-cli` is an HTTP client that links
    // NONE of the backend crates; its `WAKE_STORES` names `org-manifest` as a
    // `stores=` changefeed filter — a WIRE value, in exactly the sense a route
    // path is one. It has no typed accessor to go through and could not use one
    // if it wanted to, so the guard's own remedy ("go through the typed
    // accessors") is unavailable by construction. Scanning it produced a
    // failure with no correct fix, which is the state that gets a guard
    // weakened. Scoping by the dependency edge keeps the rule at full strength
    // where it means something.
    let mut crate_dirs: Vec<PathBuf> = fs::read_dir(&root)
        .expect("crates/ is readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| {
            let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name == "chiefd-core" {
                return true;
            }
            // Comments stripped FIRST. `chief-cli/Cargo.toml` carries a long
            // note about deliberately NOT depending on `chiefd-core`, and a
            // plain `contains` matched the explanation of the absence as
            // though it were the dependency. That is the third checker in this
            // workstream to fail on its own prose; a matcher that greps a file
            // for a name will hit the sentence saying why the name is not
            // there.
            fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|manifest| {
                manifest
                    .lines()
                    .map(|line| line.split('#').next().unwrap_or(""))
                    .any(|code| code.contains("chiefd-core"))
            })
        })
        .map(|dir| dir.join("src"))
        .filter(|src| src.is_dir())
        .collect();
    crate_dirs.sort();
    assert!(
        crate_dirs.len() >= 4,
        "expected every chiefd-core-linking crate to be scanned: {crate_dirs:?}"
    );

    let mut files = Vec::new();
    for dir in &crate_dirs {
        walk(dir, &mut files);
    }
    files.sort();
    assert!(!files.is_empty(), "found no sources to scan — the guard would pass vacuously");

    files
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap_or_default();
            let production = code_lines(text.split("#[cfg(test)]").next().unwrap_or(&text));
            let relative =
                path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            (relative, production)
        })
        .collect()
}

/// The module a store's rows may be reached from, plus the registry that names
/// every store by definition.
fn allowed_files(store: StoreId) -> Vec<&'static str> {
    let owner = match store {
        StoreId::LaunchIntent => "chiefd-core/src/store/launch_intent.rs",
        StoreId::Health => "chiefd-core/src/store/health.rs",
        StoreId::SessionMaintenance => "chiefd-core/src/store/session_maintenance.rs",
        StoreId::Organization => "chiefd-core/src/store/organization.rs",
        StoreId::Activity => "chiefd-core/src/store/activity.rs",
        StoreId::Supervision => "chiefd-core/src/store/supervision.rs",
        StoreId::SupervisorWatermark => "chiefd-core/src/store/supervisor_watermark.rs",
        StoreId::ConvergeSafety => "chiefd-core/src/store/converge_safety.rs",
    };
    // `store/mod.rs` is the registry: it holds the plan §5.5 inventory, which
    // is a list of store names and cannot avoid naming them. Widening this
    // list further needs the same argument in writing.
    let mut allowed = vec![owner, "chiefd-core/src/store/mod.rs"];

    // `store/reconciler_facts.rs` is permitted for the `launch-intent` KEY only:
    // it is the module's whole reason to exist. The launch-intent ledger's
    // sole WRITER is still the TypeScript staffing lifecycle, whose rows live
    // in the shared legacy `org_documents` table — the exact gap this reader
    // module covers (its module docs carry the standing list). The converge
    // cycle's activity-fence projection must read that row, and the guard's
    // own rationale is honored rather than bypassed: the reader decodes the
    // row through the store's own `LaunchIntentBody` and reproduces the
    // store's fail-safe polarity (absent/corrupt/foreign ⇒ the restrictive
    // empty fence), so nothing here reaches the row WITHOUT the polarity the
    // store was given. It is read-only; no write path exists. Scoped to this
    // one store — the other rows this module reads have no typed native
    // store, so they never reach this match arm at all.
    if store == StoreId::LaunchIntent {
        allowed.push("chiefd-core/src/store/reconciler_facts.rs");
        // `store/launch_intent_rows.rs` is the store's OWN normalized-rows module
        // (org-data-normalization P0, N-B4): the columnar port of this store. It
        // names the `launch-intent` key as its own `LAUNCH_INTENT_STORE` const and
        // as the `org_events` entity-kind its typed accessors stamp — the store
        // naming ITSELF, which is exactly what this guard permits ("no source
        // OUTSIDE a store's own module"). It is a co-owner of the launch-intent
        // concept alongside `launch_intent.rs` (the pre-port blob reader), not a
        // bypass: `org_ops` now composes its typed `delete_person_fence` accessor
        // rather than hand-rolling `DELETE FROM launch_intent` (#32 containment).
        allowed.push("chiefd-core/src/store/launch_intent_rows.rs");
    }
    // Normalized-rows port submodules/modules (org-data-normalization P0). Each is
    // the store's OWN module — the columnar persistence half of the same store —
    // so naming its own key is exactly what this guard permits ("no source OUTSIDE
    // a store's own module"), not a bypass: the typed accessors these expose ARE
    // the polarity-preserving path callers must use. Registered as owners here the
    // same way the pre-port module already is.
    if store == StoreId::Activity {
        allowed.push("chiefd-core/src/store/activity/rows.rs");
    }
    if store == StoreId::Supervision {
        allowed.push("chiefd-core/src/store/supervision/rows.rs");
    }
    // BLOB-DEATH (org-data-normalization P0, N8): `organization_rows.rs` is the
    // org-manifest store's own normalized-rows module -- the columnar
    // persistence half `persist_dispatch`/`load_ledgers` dispatch the
    // documents key to/reconstruct it from. Same pattern as the Activity /
    // Supervision rows submodules above:
    // the store naming ITSELF, not a bypass.
    if store == StoreId::Organization {
        allowed.push("chiefd-core/src/store/organization_rows.rs");
    }
    // F16 un-cross-wiring (arch Step 4): the daemon's health duty store is now
    // "health-monitor", persisted by `health_monitor_rows.rs` — the same row
    // module the TS launcher's health monitor publishes through (merge
    // semantics, Step 3). The store naming ITSELF, not a bypass.
    if store == StoreId::Health {
        allowed.push("chiefd-core/src/store/health_monitor_rows.rs");
    }
    // BLOB-DEATH: `supervisor_watermark_rows.rs` is the supervisor-watermark
    // store's own normalized-rows module -- the columnar persistence half
    // persist_dispatch/load_ledgers dispatch the documents key to/reconstruct
    // it from. Daemon-internal store, same pattern as the rows submodules
    // above: the store naming ITSELF, not a bypass.
    if store == StoreId::SupervisorWatermark {
        allowed.push("chiefd-core/src/store/supervisor_watermark_rows.rs");
    }
    // BLOB-DEATH: `converge_safety_rows.rs` is the converge-safety store's
    // own normalized-rows module, same pattern as the rows submodules above.
    if store == StoreId::ConvergeSafety {
        allowed.push("chiefd-core/src/store/converge_safety_rows.rs");
    }
    allowed
}

#[test]
fn no_source_outside_a_stores_own_module_can_name_its_documents_key() {
    for &store in StoreId::ALL {
        let literal = format!("\"{}\"", store.name());
        let allowed = allowed_files(store);
        let offenders: Vec<String> = production_sources()
            .iter()
            .filter(|(path, text)| !allowed.contains(&path.as_str()) && text.contains(&literal))
            .map(|(path, _)| path.clone())
            .collect();
        assert!(
            offenders.is_empty(),
            "'{}' is reachable from {offenders:?}. The documents key is the whole \
             bypass: naming it lets a caller read or write the store without the \
             polarity that store was given. Go through the typed accessors.",
            store.name()
        );
    }
}

#[test]
fn no_source_outside_a_stores_own_module_can_name_its_store_type() {
    // The `NAME` associated const is the other route to the same key.
    for (store, ty) in [
        (StoreId::LaunchIntent, "LaunchIntentStore"),
        (StoreId::Health, "HealthStore"),
        (StoreId::SessionMaintenance, "SessionMaintenanceStore"),
        (StoreId::Organization, "OrganizationStore"),
        (StoreId::Activity, "ActivityStore"),
        (StoreId::Supervision, "SupervisionStore"),
        (StoreId::SupervisorWatermark, "SupervisorWatermarkStore"),
        (StoreId::ConvergeSafety, "ConvergeSafetyStore"),
    ] {
        let allowed = allowed_files(store);
        let offenders: Vec<String> = production_sources()
            .iter()
            .filter(|(path, text)| !allowed.contains(&path.as_str()) && text.contains(ty))
            .map(|(path, _)| path.clone())
            .collect();
        assert!(offenders.is_empty(), "{ty} escaped its module into {offenders:?}");
    }
}

#[test]
fn the_launch_intent_type_has_no_permissive_variant() {
    let sources = production_sources();
    let (_, module) = sources
        .iter()
        .find(|(path, _)| path == "chiefd-core/src/store/launch_intent.rs")
        .expect("the launch-intent module exists");

    let body = module
        .split_once("\npub enum LaunchIntent {")
        .map(|(_, rest)| rest.split_once('}').map_or(rest, |(inside, _)| inside))
        .expect("LaunchIntent is declared as an enum");

    let variants: Vec<&str> = body.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    assert_eq!(
        variants,
        vec!["Fenced(BTreeSet<String>),"],
        "LaunchIntent must have exactly one variant. A second variant — however it is \
         spelled — is the 'absence means no fence' bug returning, and it is one \
         refactor away from projecting a 28-agent fleet."
    );

    // Belt and braces: no `Option<LaunchIntent>` anywhere. Wrapping the fence
    // in an Option reintroduces exactly the optionality the enum removes.
    for (path, text) in &sources {
        assert!(
            !text.contains("Option<LaunchIntent>"),
            "{path} wraps the fence in an Option; `None` would be a fence-shaped hole"
        );
    }
}

/// The one file allowed to spell a permissive fence, and why.
///
/// **M7 banned the spelling outright and said "if one is genuinely needed, it
/// belongs in the plan first". M12 needed one, so the plan was amended first**
/// (README §5.5, "The durable fence and the reconcile input are different
/// types"), and this exemption records the outcome rather than quietly
/// widening the rule.
///
/// The distinction the original ban could not express, because it greps for a
/// token rather than a type:
///
/// * `LaunchIntent` — the **durable** fence, read from a store. It has exactly
///   one variant and no permissive form, and every assertion below still holds
///   for it, unchanged.
/// * `LaunchFence` — the **reconcile input**, supplied per call by the
///   supervisor. The TypeScript has an explicit unfenced mode here
///   (`UNFENCED_LAUNCH_INTENT`), the corpus pins it
///   (`inv-c1-unfenced-requires-the-explicit-sentinel`), and removing it would
///   make chiefd unable to reproduce recorded behaviour.
///
/// The hazard M7 was actually defending against — *absence* meaning *no fence*
/// — is unchanged and is asserted positively by
/// `a_launch_fence_can_never_become_permissive_by_omission` below: the
/// permissive variant is reachable only by naming it.
const PERMISSIVE_FENCE_EXEMPTION: &str = "chiefd-core/src/store/activity.rs";

/// Second sanctioned exemption, amended into the plan alongside this const
/// (see the commit that added it): `cycle.rs` legitimately constructs the
/// `Unfenced` sentinel for the documented cold-restart reason-pass
/// substitution (`ConvergeActuator::reconcile`'s `None => ... fence:
/// LaunchFence::Unfenced` branch), landed with the P1 stop/attach fold
/// (13db7512) — this is the actuator CONSUMING the sentinel per that fold's
/// own design, not a new escape hatch. This is the type-carried,
/// explicitly-named permissive fence the milestone's own positive test
/// (`a_launch_fence_can_never_become_permissive_by_omission`) already proves
/// is unreachable by omission; `cycle.rs` naming it is not the footgun this
/// guard exists to catch.
const SECOND_PERMISSIVE_FENCE_EXEMPTION: &str = "chiefd-host/src/converge_apply/cycle.rs";

#[test]
fn no_unfenced_escape_hatch_has_been_reintroduced_under_any_historical_spelling() {
    // The two TS sentinels and the identifiers built from them. None of these
    // has any business existing in Rust: the type system carries the fence.
    let banned_everywhere = [
        "launch-intent:unfenced",
        "unfenced-launch-intent:no-fence",
        "LAUNCH_INTENT_UNFENCED",
        "UNFENCED_LAUNCH_INTENT",
    ];
    for (path, text) in production_sources() {
        for needle in banned_everywhere {
            assert!(
                !text.contains(needle),
                "{path} names '{needle}'. The Rust fence has no unfenced form; if one is \
                 genuinely needed, it belongs in the plan first — the predecessor's \
                 default-allow footgun is what this milestone exists to remove."
            );
        }
        assert!(
            !text.contains("Unfenced")
                || path == PERMISSIVE_FENCE_EXEMPTION
                || path == SECOND_PERMISSIVE_FENCE_EXEMPTION,
            "{path} names 'Unfenced'. Only PERMISSIVE_FENCE_EXEMPTION and \
             SECOND_PERMISSIVE_FENCE_EXEMPTION may — amend the plan before \
             adding a third."
        );
    }
}

/// The property the ban was defending: a permissive fence must be reachable
/// only by *naming* it, never by dropping a field or passing nothing.
///
/// This is the positive half of the exemption above. It fails if
/// `LaunchFence` ever grows a `Default`, a `From<Option<…>>`, or any other
/// route by which absence could resolve to "everyone runs".
#[test]
fn a_launch_fence_can_never_become_permissive_by_omission() {
    use chiefd_core::store::activity::LaunchFence;

    // The two ways a caller can express "no allow-list", and neither is
    // permissive.
    assert_eq!(LaunchFence::deny_all(), LaunchFence::Fenced(Default::default()));
    assert_ne!(LaunchFence::deny_all(), LaunchFence::Unfenced);
    assert_ne!(
        LaunchFence::fenced(Vec::<String>::new()),
        LaunchFence::Unfenced,
        "an explicit empty allow-list is CEO-only, exactly like omission"
    );

    let source = include_str!("../src/store/activity.rs");
    let production = source.split("#[cfg(test)]").next().unwrap_or(source);
    assert!(
        !production.contains("impl Default for LaunchFence"),
        "a Default for LaunchFence is 'absence means something' by another name"
    );
    assert!(
        !production.contains("Option<LaunchFence>"),
        "wrapping the reconcile fence in an Option makes `None` a fence-shaped hole"
    );
}

/// The exemption above is only safe while every out-of-line `tests.rs` is
/// genuinely test-only. A `mod tests;` that lost its `#[cfg(test)]` would ship
/// into the binary AND be skipped by the production scan — the worst of both.
#[test]
fn every_out_of_line_test_module_is_cfg_test_gated() {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|name| name == "tests.rs") {
                out.push(path);
            }
        }
    }

    let root = crates_root();
    let mut modules = Vec::new();
    for entry in fs::read_dir(&root).expect("crates/ is readable").flatten() {
        walk(&entry.path().join("src"), &mut modules);
    }
    assert!(!modules.is_empty(), "no out-of-line test modules found — has the layout changed?");

    for module in modules {
        // The declaring file is the sibling `<parent>.rs` or `<parent>/mod.rs`.
        let parent = module.parent().expect("a tests.rs has a parent directory");
        let sibling = parent.with_extension("rs");
        let mod_rs = parent.join("mod.rs");
        let declaration = [sibling, mod_rs]
            .into_iter()
            .find_map(|candidate| fs::read_to_string(&candidate).ok())
            .unwrap_or_else(|| panic!("cannot find the module declaring {}", module.display()));
        let gated = declaration
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair[0].trim() == "#[cfg(test)]" && pair[1].trim() == "mod tests;");
        assert!(
            gated,
            "{} is declared without #[cfg(test)]: it would ship in the binary and \
             still be skipped by the production scan",
            module.display()
        );
    }
}

#[test]
fn every_store_type_in_the_crate_is_in_the_registry() {
    // A `StoreKind` impl outside the registry cannot compile (the seal), but it
    // can be *written*, and a confusing compile error is a worse signal than
    // this message. Counting them also catches a store declared in the macro
    // but implemented against a type that already existed for another purpose.
    let impls: usize = production_sources()
        .iter()
        .map(|(_, text)| text.matches("impl StoreKind for").count())
        .sum();
    assert_eq!(
        impls,
        StoreId::ALL.len(),
        "every StoreKind impl must correspond to exactly one registry entry"
    );
}

#[test]
fn the_store_names_the_registry_reports_are_the_ones_the_types_carry() {
    assert_eq!(StoreId::LaunchIntent.name(), LaunchIntentStore::NAME);
}

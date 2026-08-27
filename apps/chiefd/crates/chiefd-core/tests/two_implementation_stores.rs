// `clippy.toml`'s `allow-expect-in-tests` only reaches functions carrying
// `#[test]`; the helpers below are test scaffolding by construction, and a
// failed `expect` here IS the test failing. Same allowance every sibling
// architectural test takes (see `fence_containment.rs:5`).
#![allow(clippy::expect_used, clippy::panic)]
//! Architectural guard: one concept must not end up with two durable
//! implementations across the chiefd migration (#81). Every native relational
//! table must have a recorded owner; a native writer TypeScript also owns must
//! stay callerless; and every boot-seeded store must have exactly one typed row
//! authority.
//!
//! # The bug this exists to make impossible
//!
//! On 2026-07-24, seven separate bugs turned out to be one shape: **chiefd
//! migrated a store and one side stayed behind.** Two mailboxes (#79), two
//! supervision implementations (#67), two document stores (#37/#442), two id
//! schemes for one entity (#87), two renderers where the buggy one had no
//! consumer (#76), diverging revision semantics (#85), and two stores
//! implementing one concept (#22).
//!
//! The mechanism was two different physical tables:
//!
//! * the chiefd-core ledger `documents` — reached by `Ledgers::put_document`,
//!   keyed by store name alone, one database per company; and
//! * the shared `org_documents` — what the TypeScript `durableStore()` wrote
//!   and what `/v1/docs/*` served, keyed `(slug, store)` where the slug is the
//!   composite `<slug>@<first 12 hex of sha256(dataRoot)>`.
//!
//! A write to one was **never** visible to a reader of the other. When both
//! sides used the same store NAME, that invisibility is exactly what made the
//! bug survive review: the names matched, so it read as "migrated cleanly".
//!
//! # The name-collision half of this file is gone, and why that is not a
//! weakening
//!
//! Both tables are dead. `schema.rs` creates neither `documents` nor
//! `org_documents` (it asserts the first is absent, in
//! `documents_and_vestigial_provider_admission_are_absent_after_final_cutover`),
//! and the TypeScript writer that owned the second — `durableStore()` /
//! `org-durable-store.ts` — was deleted with the rest of the ported modules by
//! #751. TypeScript now reaches durable state ONLY through chiefd's typed
//! `/v1/org/<store>/<operation>` row routes, which land in chiefd's one
//! normalized row set. There is no second physical table for a store name to
//! fork across, so a NAME collision can no longer mean two copies.
//!
//! The collision guard and its `BRIDGED_OR_ACCEPTED` reason list were therefore
//! DELETED rather than repointed. This is the outcome the control below already
//! prescribed in writing ("AT THE FINISH LINE … this whole control is deleted
//! with the blob"), and the alternative was the failure this file exists to
//! prevent: a scan whose subject no longer exists reports its empty
//! intersection as SAFETY. That is exactly how it surfaced — the TypeScript
//! scan's own `assert!(names.contains("supervision"))` self-check fired,
//! refusing to hand back a zero it could not justify. The self-check earned its
//! keep twice: once for #787's moved root (repointed, DECISIONS 2026-08-04) and
//! once here, where the honest answer was that there is nothing left to scan.
//!
//! What survives is everything whose subject still exists: the relational-table
//! ownership registry (the dimension a name-based scan never covered), the
//! loaded-gun guard on native writers with no production caller, the native
//! scan's own not-silently-empty control, and the typed-row read/publish
//! authority check over the boot-seeded stores.
//!
//! # Why an architectural test rather than care
//!
//! Every instance found by an operator complaint cost hours. The two found by
//! the #81 audit cost nothing, because they had not fired yet — and the reason
//! they had not fired is that nobody had wired a caller. **Care does not scale
//! to the person who wires the caller in six months.** This test does.
//!
//! Same move `reconciler_facts.rs` makes for its eight bridged facts and the
//! `observe_scaffolding_is_isolated` guard makes for the observe scaffolding:
//! assert the architecture against source text, and make the failure message
//! carry the reasoning so whoever trips it inherits it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// `apps/chiefd/crates`.
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
    manifest.parent().expect("crates/chiefd-core has a parent").to_path_buf()
}

/// Drop `//` comment lines: this guard is about what the compiler and the
/// runtime see, and documentation must stay free to NAME the hazard. An
/// undocumented hazard is how it comes back.
fn code_lines(text: &str) -> String {
    text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// Store names whose chiefd-core module writes the NATIVE `documents` table.
///
/// Coarse on purpose: if a store module calls `put_document` at all outside its
/// tests, that module writes natively, and its `NAME` is what it writes under.
fn natively_written_store_names() -> BTreeMap<String, String> {
    let store_dir = crates_root().join("chiefd-core/src/store");
    let mut names = BTreeMap::new();
    let entries = fs::read_dir(&store_dir).expect("chiefd-core/src/store is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        // Everything from `mod tests` onward is test scaffolding, not a writer.
        let production = text.split("mod tests").next().unwrap_or("").to_string();
        let production = code_lines(&production);
        if !production.contains("put_document(") && !production.contains("remove_document(") {
            continue;
        }
        let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        for name in store_names_declared_in(&production) {
            names.insert(name, file.clone());
        }
    }
    assert!(
        !names.is_empty(),
        "found no natively-written store names — the Rust scan is broken, and a broken scan \
         reports an empty intersection as SAFETY."
    );
    names
}

/// Resolve every store name a module declares: `const NAME: &'static str =
/// "literal"` directly, or via a `const X: &str = "literal"` in the same file.
fn store_names_declared_in(production: &str) -> Vec<String> {
    let mut literals: BTreeMap<String, String> = BTreeMap::new();
    let mut names = Vec::new();
    for line in production.lines() {
        let trimmed = line.trim();
        if let Some(rest) =
            trimmed.strip_prefix("pub const ").or_else(|| trimmed.strip_prefix("const "))
        {
            let Some((ident, value)) = rest.split_once('=') else { continue };
            let ident = ident.split(':').next().unwrap_or("").trim().to_string();
            if let Some(literal) = value.trim().strip_prefix('"').and_then(|v| v.split('"').next())
            {
                literals.insert(ident, literal.to_string());
            }
        }
    }
    for line in production.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("const NAME: &'static str =") else { continue };
        let value = rest.trim().trim_end_matches(';').trim();
        if let Some(literal) = value.strip_prefix('"').and_then(|v| v.split('"').next()) {
            names.push(literal.to_string());
        } else if let Some(resolved) = literals.get(value) {
            names.push(resolved.clone());
        }
    }
    names
}

/// Rust source that ships in the daemon (not chiefd-core's own store modules,
/// and not tests) — where "someone wired a caller" would appear.
fn daemon_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for crate_dir in ["chiefd/src", "chiefd-host/src", "chiefd-api/src"] {
        collect_rust(&crates_root().join(crate_dir), &mut files);
    }
    files
}

fn collect_rust(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// The guard that actually fires.
///
/// `launch_intent::clear` is a LOADED GUN with no production caller. It fires
/// at the moment somebody wires one, not after the outage.
///
/// **Read what this guards today, not what it guarded in 2026-07.** The
/// original argument was a two-table divergence: chiefd would write the native
/// `documents` ledger while the launcher read the `org_documents` row, and
/// neither side would see the other. That mechanism is retired —
/// `launch_intent::add` graduated off this list with BLOB-DEATH for exactly
/// that reason, and `clear` now takes the same route: `remove_document` on the
/// launch-intent key dispatches through `persist_dispatch::dispatch_clear` into
/// `launch_intent_rows::clear`, a REAL delete of the same normalized rows the
/// launcher reads. So a wired `clear` would be VISIBLE to the launcher, not
/// invisible.
///
/// What is left to guard is #90's mutual-exclusion fence itself: `clear` drops
/// a whole company's launch fence in one statement, and chiefd is a READER of
/// that fence, not its owner. Whether that still warrants a callerless-writer
/// guard — or whether `clear` should graduate off this list the way `add` did —
/// is an open call for whoever closes #90. It is recorded here rather than
/// silently kept, because a guard that passes for a reason that stopped being
/// true is the same false green this file exists to prevent.
#[test]
fn the_unbridged_native_stores_still_have_no_production_caller() {
    const LOADED: &[(&str, &str)] = &[(
        "launch_intent::clear",
        "#90 — the mutual-exclusion FENCE; chiefd READS it, the launcher owns it",
    )];

    let mut wired: Vec<String> = Vec::new();
    for file in daemon_sources() {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        let production = code_lines(text.split("mod tests").next().unwrap_or(""));
        for (needle, why) in LOADED {
            if production.contains(needle) {
                let shown = file.strip_prefix(crates_root()).unwrap_or(&file).display();
                wired.push(format!("  - {needle} called from {shown}\n      {why}"));
            }
        }
    }

    assert!(
        wired.is_empty(),
        "A callerless native writer over a store chiefd only READS just gained a production \
         caller:\n{}\n\n\
         Until now this was inert, so it could not act. `launch_intent::clear` drops a whole \
         company's launch fence in one statement, and the fence is #90's mutual-exclusion \
         authority — chiefd reads it, it does not own it.\n\n\
         Before wiring this, resolve #90: settle who owns the fence, and say in writing why a \
         wholesale clear is the right verb for that owner. Then delete the corresponding entry \
         here, with the reason — the way `launch_intent::add` graduated off this list.",
        wired.join("\n")
    );
}

/// The scans must be able to FIND something before an empty result is read as
/// safety. Three false zeros on 2026-07-24 came from patterns that could not
/// match what they were looking for — so the control runs on the query itself,
/// not only on the data.
///
/// The TypeScript half of this control, and the native∩TS overlap assertion it
/// fed, are gone: `org_documents` and its `durableStore()` writer no longer
/// exist, so a TS store-name scan has no subject and its zero would have been a
/// false negative dressed as green (module docs). Its self-check is what
/// surfaced that, and the file's own instruction for the finish line was to
/// delete the control with the blob rather than keep scanning nothing. The
/// native scan below still HAS a subject and keeps its full self-check.
#[test]
fn the_scans_can_actually_match_and_are_not_silently_empty() {
    let native = natively_written_store_names();
    assert!(native.len() > 5, "native scan found only {}: {native:?}", native.len());
    for expected in ["supervision", "activity", "launch-intent"] {
        assert!(native.contains_key(expected), "native scan missed {expected}: {native:?}");
    }
    // Step 9 (F11 corrected): the shadow twin is gone, so the scan must NOT
    // find session-maintenance among native blob writers — its presence here
    // again would mean somebody regrew a second writer.
    assert!(
        !native.contains_key("session-maintenance"),
        "session-maintenance sprouted a native blob writer again (the deleted shadow twin): {native:?}"
    );
}

/// # The blind spot this second dimension closes
///
/// The retired collision guard keyed on a store NAME, so it only ever saw
/// stores that lived in the `documents` table. **It would NOT have caught
/// #79** — chiefd stages mail in a native RELATIONAL `mailbox` table, which has
/// no store name at all, while the pane drains a `mailbox/<personId>` DOCUMENT.
/// A guard with a blind spot that reads as total coverage is worse than no
/// guard, so the relational tables get their own registry: every native table
/// must be declared here with who owns it and whether TypeScript has a
/// counterpart.
///
/// This registry OUTLIVED the name-based guard, and that is the point of
/// keeping the two dimensions separate: the document tables died with the blob,
/// while every durable fact now lives in exactly the relational tables this
/// list covers. It is the live half of this file.
///
/// This does not prove a table is safe. It proves somebody LOOKED — and it
/// fails when a new native table appears with nobody having answered the
/// question, which is exactly how #79 got in.
const NATIVE_RELATIONAL_TABLES: &[(&str, &str)] = &[
    (
        "documents",
        "The native document ledger itself. Retired with the blob — `schema.rs` no longer creates \
         it and asserts its absence — and the row stays as the historical anchor for the \
         name-collision hazard the module docs record.",
    ),
    (
        "effects",
        "chiefd-owned. Deliberately `#[serde(skip)]` out of the supervision document body — the          relational rows are the authority and the counter is derived, never decoded (#37).",
    ),
    (
        "effect_payloads",
        "chiefd-owned normalized child rows of `effects`: one typed scalar or ordered scalar-array \
         value per (slug, effect_id, field, ordinal). TypeScript never reads or writes this table \
         directly; it publishes and reconstructs effects through the typed supervision row \
         route, using the same effect id as the parent row.",
    ),
    ("counters", "chiefd-internal sequence counters. No TypeScript counterpart."),
    (
        "mailbox",
        "⚠️ KNOWN FORK, tracked by #79: chiefd stages envelopes in THIS relational table while          the pane drains the `mailbox/<personId>` DOCUMENT. The two use different id schemes          (native keys on the effect id, the document on the message id), which is why a search          by the wrong id returned a confident zero (#87). This is the exact case the name-based          collision guard cannot see, and the reason this registry exists.",
    ),
    (
        "organization_model_defaults",
        "Rust chiefd is the sole durable authority for one organizational provider/model default, \
         keyed by company `slug`. The TypeScript static model catalog is neither a persisted \
         company default nor a second writer; future callers must use the typed Rust model API.",
    ),
    (
        "model_change_preparations",
        "chiefd-owned two-phase live model change audit: Rust prepares an exact observed \
         provider/model route (keyed by `(slug, change_id)`), Pi applies it, then Rust commits \
         only while the current route/runtime fence is unchanged. TypeScript has no counterpart \
         writer or reader; this is a Rust-internal audit trail, not a document-bridged store.",
    ),
    (
        "provider_model_observations",
        "Rust chiefd is the sole authority for the latest provider observation, keyed by the \
         exact `(slug, provider)` pair with a Rust-owned generation. TypeScript currently has \
         static/host discovery code but no relational writer or matching durable ID scheme.",
    ),
    (
        "provider_models",
        "Rust chiefd-owned normalized children of `provider_model_observations`, keyed by \
         `(slug, provider, model_id)`. TypeScript has no table counterpart; model ids originate \
         in the owning provider snapshot and are never joined through a static catalog id.",
    ),
    (
        "provider_model_modalities",
        "Rust chiefd-owned ordered modality children, keyed by \
         `(slug, provider, model_id, modality)` under the provider-model parent. TypeScript has \
         no relational counterpart and no independently writable modality identity.",
    ),
    (
        "provider_model_observation_ids",
        "Rust chiefd-internal bounded idempotency proofs, keyed by \
         `(slug, provider, observation_id)` and tied to Rust-owned generations/payload hashes. \
         TypeScript may mint an opaque observation id but never stores or owns these proof rows.",
    ),
    ("leases", "chiefd-internal lease rows (`lease.rs`). No TypeScript counterpart."),
    ("host_actions", "chiefd-internal host actuation journal. No TypeScript counterpart."),
    ("ack_receipts", "chiefd-owned normalized rows for the acks port (org-data-normalization P0). The `acks` document is reconstructed from these rows and served on `/v1/org/acks/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("activity_meta", "chiefd-owned normalized rows for the activity port (org-data-normalization P0). The `activity` document is reconstructed from these rows and served on `/v1/org/activity/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("converge_safety", "chiefd-internal converge-safety rows. No TypeScript counterpart."),
    (
        "runtime_actuation",
        "chiefd-internal actuation record (#751/P8): who is actuating this company, when they \
         last reported, and whether their observation is trusted. No TypeScript counterpart — the \
         consumer is the Rust operator client, which speaks HTTP directly. chiefd is \
         AUTHORITATIVE on the desired set and on this record; the actuator is authoritative on \
         nothing but its own observation, which it may only assert through the route. ID scheme \
         is the company slug, the same singleton key `converge_safety` uses.",
    ),
    (
        "runtime_actuation_people",
        "The people half of the actuation record, keyed (slug, person_id) — the SAME person id \
         scheme as `organization`/`activity`, deliberately, because a second id scheme for the \
         same person is exactly the #79 hazard this registry exists to catch. Written only for a \
         trusted observation and deleted wholesale on every report.",
    ),
    (
        "runtime_actuation_unknown",
        "Processes the actuator could not attribute, keyed (slug, pid). Deliberately NOT keyed by \
         person: an unattributed process has no person, and inventing one would let chiefd emit \
         a stop for a pid it cannot name (#438). Reported only; never acted on.",
    ),
    ("supervisor_watermarks", "chiefd-internal supervisor duty watermark rows (last_success_at/run_count per duty). No TypeScript counterpart."),
    ("departments", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("event_once_markers", "chiefd-internal idempotence markers (delta #29 rename of the retired typed-blob event_markers). No TypeScript counterpart."),
    (
        "event_journal_sweep",
        "chiefd-internal, and there is nothing on the other side to diverge from. This is the \
         per-company throttle stamp for the reactive once-marker sweep \
         (`store/event_journal.rs`). Its TypeScript predecessor kept the same value in a \
         process-local `Map<slug, lastSweptMs>` — never durable, never shared, and a Mandate 2 \
         violation — and that module is deleted, so the port did not create a second store, it \
         moved process memory into SQL. AUTHORITATIVE: Rust chiefd, solely; it is read and \
         written in the SAME transaction as the marker insert and the prune it may trigger. ID \
         SCHEME: the company `slug` alone (PRIMARY KEY), the same company key every other row \
         store here is keyed by, so there is no second identity to drift.",
    ),
    ("health", "chiefd-owned health rows (N7-fable, schema present; readers land with n7-fable). No live TypeScript counterpart yet."),
    ("health_monitor_cursors", "chiefd-owned health-monitor rows (N7-fable). Sibling of health_monitor_meta."),
    ("health_monitor_incidents", "chiefd-owned health-monitor rows (N7-fable). Sibling of health_monitor_meta."),
    ("health_monitor_meta", "chiefd-owned health-monitor rows (N7-fable). No live TypeScript counterpart yet."),
    ("health_monitor_observations", "chiefd-owned health-monitor rows (N7-fable). Sibling of health_monitor_meta."),
    ("health_monitor_terminal_resolutions", "chiefd-owned health-monitor rows (N7-fable). Sibling of health_monitor_meta."),
    (
        "identities",
        "agent-auth (P0) cryptographic caller identities. chiefd/Rust is the SOLE authority: the \
         verify-middleware reads it on every /v1 request and only chiefd writes it (bootstrap \
         self-enrol + the authenticated /v1/auth/enroll handler). TypeScript does NOT own or \
         mirror this concept — the launcher generates keypairs and calls the enrol ENDPOINT; it \
         never touches the table. ONE id scheme: `identity_id` (the raw person id a pane presents, \
         or `operator`/channel names) — the exact value the JWT `sub` carries, so there is no \
         cross-side id-scheme drift of the #79 class. Company-owned: one chiefd process and one \
         company database share this identity set.",
    ),
    ("launch_intent", "chiefd-owned normalized rows for the launch-intent port (org-data-normalization P0). The `launch-intent` document is reconstructed from these rows and served on `/v1/org/launch-intent/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    // TOMBSTONE: `maintenance_company_action_targets` was declared here beside
    // its parent. It is dropped from `schema.rs` with `org_maintain_session`,
    // so the declaration is deleted with it — this registry is scanned FROM the
    // schema, so a stale row here would never fail the assertion below, it
    // would just claim an owner for a table that does not exist (the #963
    // shape, same as the `reflection_handoffs` and `unit_removals` tombstones
    // below). Its PARENT survives and keeps its row: `maintenance_company_actions`
    // is retained EMPTY because `maintenance_requests` foreign-keys onto it and
    // SQLite resolves the parent when the child INSERT prepares.
    ("maintenance_company_actions", "chiefd-owned normalized rows for the session-maintenance port (org-data-normalization P0). The `session-maintenance` document is reconstructed from these rows and served on `/v1/org/session-maintenance/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("maintenance_ledger", "chiefd-owned normalized rows for the session-maintenance port (org-data-normalization P0). The `session-maintenance` document is reconstructed from these rows and served on `/v1/org/session-maintenance/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    // TOMBSTONE: `maintenance_request_models` held the provider/model payload of
    // a `set_model` maintenance request, declared here as the one table whose
    // authority split was "Rust is sole durable authority; TypeScript sends and
    // projects but never reads or writes". `set_model` is deleted and the table
    // is dropped from `schema.rs`, so the declaration goes with it — a stale row
    // here is silent, not loud (see the tombstone above).
    ("maintenance_requests", "chiefd-owned normalized rows for the session-maintenance port (org-data-normalization P0). The `session-maintenance` document is reconstructed from these rows and served on `/v1/org/session-maintenance/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("mutation_journal", "chiefd-internal in-flight mutation journal (schema delta #28/#29; N7-fable tables present, readers land with n7-fable). No TypeScript counterpart."),
    ("operator_escalation_intents", "chiefd-owned normalized rows for the operator-escalation-intents port (org-data-normalization P0). The `operator-escalation-intents` document is reconstructed from these rows and served on `/v1/org/operator-escalation-intents/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    (
        "operator_escalation_log",
        "chiefd-owned durable record of an out-of-band operator escalation (`store/\
         operator_escalation.rs`), served read-only on `/v1/org/operator-escalation-log/read`. \
         TypeScript owned this concept and no longer does: the retired producer appended \
         `logs/operator-escalations.jsonl` behind a separate `appendOrganizationJournalEventOnce` \
         marker — two writes to two stores for one fact, with the file half banned by Mandate 5 — \
         and both TypeScript modules are deleted. AUTHORITATIVE: Rust chiefd, and there is no \
         second copy left to disagree with it. ID SCHEME: `(slug, fingerprint)` PRIMARY KEY, where \
         the fingerprint is the escalation's own deterministic identity. That is deliberately the \
         SAME fingerprint `operator_escalation_intents` keys on and `operator_escalation_push` \
         carries in `pending_fingerprint`, so the three tiers join on one id rather than the \
         marker-id/append-record pair the retired file tier used — which is exactly the #79-class \
         split (one concept, two id schemes) this registry exists to force somebody to check.",
    ),
    ("operator_escalation_push", "chiefd-owned normalized rows for the operator-escalation-push port (org-data-normalization P0). The `operator-escalation-push` document is reconstructed from these rows and served on `/v1/org/operator-escalation-push/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("org_events", "chiefd-owned shared event-seq feed - the per-slug monotonic seq every normalized row store fences on (org-data-normalization P0). No TypeScript counterpart; it IS the fence."),
    ("org_settings", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("people", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("person_activity", "chiefd-owned normalized rows for the activity port (org-data-normalization P0). The `activity` document is reconstructed from these rows and served on `/v1/org/activity/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("person_contracts", "chiefd-owned normalized rows for the person-contracts port (org-data-normalization P0). The `person-contracts` document is reconstructed from these rows and served on `/v1/org/person-contracts/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("person_prompts", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    // TOMBSTONE (chief-home-is-cwd §4e): `person_resources` was declared here as
    // manifest-port rows carrying each person's selected skills, extensions and
    // packages. The table is dropped from `schema.rs` because nobody selects a
    // Pi resource for a person: an agent's skills are the files in
    // `<dir>/.pi/skills`, which Pi discovers and loads through one symlink. The
    // declaration is deleted with it — this registry is scanned FROM the schema,
    // so a stale row here would never fail the assertion below, it would just
    // claim an owner for a table that does not exist (the #963 shape, same as
    // the two tombstones below). `person_tools` KEEPS its row: a tool grant is
    // still chief's decision and still reaches a pane on argv.
    ("person_tools", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("quiesce", "chiefd-owned normalized rows for the goal-delivery-quiesce port (org-data-normalization P0). The `goal-delivery-quiesce` document is reconstructed from these rows and served on `/v1/org/goal-delivery-quiesce/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("stand_down", "chiefd-owned, and there is NO TypeScript side: the operator's company stand-down is a decision about who may RUN, which only an actuator can carry out, and the Rust operator client is the only actuator this product has. Written by `POST /v1/org/stand-down` from `chief stand-down`/`chief resume` and from the CEO's `org_stand_down`/`org_resume`, and read by the converge pass before it grants launch intent. No document blob and no dual-write: the concept is younger than the org_documents era and was never modelled in TypeScript, so there is nothing for a second implementation to disagree with."),
    // TOMBSTONE (#751-P4): `reflection_handoffs` and `reflection_handoff_items`
    // were declared here as activity-port rows. Both tables are dropped from
    // `schema.rs` along with the reflection concept, so the declarations are
    // deleted with them. This registry is scanned FROM the schema, so a stale
    // row here would never fail the assertion below — it would just sit in the
    // file claiming an owner for a table that does not exist, which is exactly
    // the #963 shape (an allowlist row orphaned by a change nobody re-read).
    // The transition's release state now lives entirely in the `transitions`
    // rows; no replacement table takes their place.
    ("reminders", "chiefd-owned normalized rows for the supervision port (org-data-normalization P0). The `supervision` document is reconstructed from these rows and served on `/v1/org/supervision/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("runtime", "chiefd-owned runtime placement rows (schema #29, N7-fable; readers land with n7-fable). TypeScript owns the runtime document today; authority resolves at the N7 cutover."),
    ("runtime_monitor_warnings", "chiefd-owned runtime-monitor warning rows (N7-fable). No live TypeScript counterpart."),
    ("runtime_owner", "chiefd-owned normalized rows for the runtime-owner port (org-data-normalization P0). The `runtime-owner` document is reconstructed from these rows and served on `/v1/org/runtime-owner/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("runtime_process_handles", "chiefd-owned runtime process-handle rows (N7-fable). Sibling of runtime."),
    ("runtime_recovery_people", "chiefd-owned runtime recovery-people rows (N7-fable). Sibling of runtime."),
    ("session_epoch", "chiefd-owned normalized rows for the session-epoch port (org-data-normalization P0). The `session-epoch` document is reconstructed from these rows and served on `/v1/org/session-epoch/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("staffing_history", "chiefd-owned normalized rows for the manifest port (org-data-normalization P0). The `manifest` document is reconstructed from these rows and served on `/v1/org/manifest/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("supervision_meta", "chiefd-owned normalized rows for the supervision port (org-data-normalization P0). The `supervision` document is reconstructed from these rows and served on `/v1/org/supervision/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    ("transitions", "chiefd-owned normalized rows for the activity port (org-data-normalization P0). The `activity` document is reconstructed from these rows and served on `/v1/org/activity/*`; chiefd owns the reconstruct/diff/BEGIN-IMMEDIATE. Under Phase-2A the `org_documents` blob is RETAINED (dual-write by design; the persist() cutover is Phase-2B, tracked)."),
    // TOMBSTONE: `unit_removals` and `unit_removal_members` were declared here
    // as the per-unit removal journal, with the authority split "tracked with
    // the org-ops removal work". Nothing was ever tracked, because nothing was
    // ever built: both tables shipped as DDL and never acquired a store module,
    // a route or a writer, and none of the three phases their CHECK admitted
    // was ever produced. Both are dropped from `schema.rs`, so the declarations
    // are deleted with them — this registry is scanned FROM the schema, so a
    // stale row here would never fail the assertion below, it would just claim
    // an owner for a table that does not exist (the #963 shape, same as the
    // `reflection_handoffs` tombstone above). The live department-removal path
    // never used them: `remove_department_tree` is one guarded transaction.
];

/// Every native relational table must be declared above.
///
/// The point is not that declaring it makes it safe — it is that a new native
/// table cannot appear without somebody answering "does TypeScript also own
/// this concept, and if so which side is authoritative". #79 got in precisely
/// because nobody was required to answer that.
#[test]
fn every_native_relational_table_has_a_recorded_owner() {
    let schema = fs::read_to_string(crates_root().join("chiefd-core/src/schema.rs"))
        .expect("chiefd-core/src/schema.rs is readable");
    let production = code_lines(schema.split("mod tests").next().unwrap_or(""));

    let mut tables: BTreeSet<String> = BTreeSet::new();
    for line in production.lines() {
        let Some(rest) = line.split("CREATE TABLE IF NOT EXISTS").nth(1) else { continue };
        let name = rest.trim().trim_end_matches('(').trim().to_string();
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            tables.insert(name);
        }
    }
    assert!(
        tables.len() > 5,
        "the schema scan found only {tables:?} — a broken scan reports an empty diff as SAFETY"
    );

    let declared: BTreeSet<&str> = NATIVE_RELATIONAL_TABLES.iter().map(|(name, _)| *name).collect();
    let undeclared: Vec<&String> =
        tables.iter().filter(|name| !declared.contains(name.as_str())).collect();

    assert!(
        undeclared.is_empty(),
        "New native relational table(s) with no recorded owner: {undeclared:?}\n\n\
         Before adding a native table, answer in NATIVE_RELATIONAL_TABLES: does TypeScript also \
         own this concept, which side is AUTHORITATIVE, and do the two use the same ID SCHEME?\n\n\
         This registry exists because the retired name-based collision guard could NOT see \
         relational tables — it would not have caught #79, where chiefd stages mail in the native \
         `mailbox` table while the pane drains a `mailbox/<personId>` document, keyed on a \
         different id. Nobody was required to answer the question, so nobody did. That guard is \
         gone with the document tables it scanned; this one is not, because every durable fact \
         now lives in exactly these tables."
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// SQL-only read-authority invariant over `BOOT_ADOPTABLE_STORES`
// ═════════════════════════════════════════════════════════════════════════════

/// Store names exposed by a typed `/v1/org/<store>/<operation>` route.
///
/// The final SQL-only architecture has no generic document read/CAS authority:
/// callers reach supervision and activity through their typed row routes.
/// Deriving this set from the registered route literals keeps the guard tied to
/// the production router rather than maintaining a second allowlist here.
fn typed_row_route_store_names(operation: &str) -> BTreeSet<String> {
    let router = crates_root().join("chiefd-api/src/docstore/router.rs");
    let text = fs::read_to_string(&router).expect("chiefd-api/src/docstore/router.rs is readable");
    let production = code_lines(text.split("mod tests").next().unwrap_or(""));

    let mut names = BTreeSet::new();
    for line in production.lines() {
        let Some(path) =
            line.split(".route(\"/v1/org/").nth(1).and_then(|rest| rest.split('"').next())
        else {
            continue;
        };
        let Some(store) = path.strip_suffix(&format!("/{operation}")) else { continue };
        if !store.is_empty() {
            names.insert(store.to_string());
        }
    }
    names
}

/// `CompanyDb` methods named `<store>_publish` — the WRITE authority for a
/// boot-seeded store.
///
/// This used to scan the router for a `/v1/org/<store>/publish` route, and the
/// publisher-route sweep deleted those: nobody called them, and the row was
/// always written in-process. The invariant was never about HTTP, though — it
/// is that a boot-seeded store is not WRITE-ONLY and has exactly ONE authority
/// for its rows. So the scan follows the authority to where it actually lives
/// rather than following the door that happened to front it.
/// Answers SNAKE_CASE method prefixes, never hyphenated store slugs. The
/// conversion runs the other way at the call site (`store.replace('-', "_")`),
/// because collapsing anything in this repo INTO a hyphen makes it a slug
/// producer, and `scripts/test/slug-producers-agree.test.mjs` requires every
/// one of those to be a named, classified producer. A test helper deriving a
/// method name is not one, so it must not look like one.
fn typed_row_publish_method_prefixes() -> BTreeSet<String> {
    let writer = crates_root().join("chiefd-core/src/actor/writer.rs");
    let text = fs::read_to_string(&writer).expect("chiefd-core/src/actor/writer.rs is readable");
    let production = code_lines(text.split("mod tests").next().unwrap_or(&text));

    let mut names = BTreeSet::new();
    for line in production.lines() {
        let Some(rest) = line.split("pub async fn ").nth(1) else { continue };
        let Some(name) = rest.split('(').next() else { continue };
        let Some(store) = name.strip_suffix("_publish") else { continue };
        if !store.is_empty() {
            names.insert(store.to_string());
        }
    }
    names
}

/// Whether `store` (a hyphenated store slug) has a `CompanyDb` write
/// authority.
fn has_write_authority(prefixes: &BTreeSet<String>, store: &str) -> bool {
    prefixes.contains(&store.replace('-', "_"))
}

/// Both scans must find both boot-seeded stores before an empty result can be
/// mistaken for safety.
#[test]
fn the_typed_row_route_scan_can_actually_match() {
    let reads = typed_row_route_store_names("read");
    let publishes = typed_row_publish_method_prefixes();
    for expected in ["supervision", "activity"] {
        assert!(reads.contains(expected), "typed read-route scan missed `{expected}`: {reads:?}");
        assert!(
            has_write_authority(&publishes, expected),
            "write-authority scan missed `{expected}`: {publishes:?}"
        );
    }
}

/// Every boot-seeded ledger must be readable through its typed row route and
/// writable through exactly one `CompanyDb` authority.
///
/// `BOOT_ADOPTABLE_STORES` retains its historical name, but after blob death it
/// no longer adopts `org_documents`: `chiefd run` deterministically seeds
/// absent supervision/activity rows from the normalized manifest. Requiring
/// both directions here prevents a newly seeded store from becoming
/// write-only or read-only, while the explicit raw-route denial prevents the
/// former second authority from being reintroduced.
#[test]
fn every_boot_seeded_store_has_one_typed_row_authority() {
    let reads = typed_row_route_store_names("read");
    let publishes = typed_row_publish_method_prefixes();
    let mut unresolved = Vec::new();
    for store in chiefd_core::store::BOOT_ADOPTABLE_STORES {
        if !reads.contains(store) || !has_write_authority(&publishes, store) {
            unresolved.push(format!(
                "  - {store}: typed read={}, write authority={}",
                reads.contains(store),
                has_write_authority(&publishes, store)
            ));
        }
    }
    assert!(
        unresolved.is_empty(),
        "Boot-seeded store(s) missing a typed row authority:\n{}\n\n\
         A deterministic startup seed is safe only when every subsequent read and write reaches \
         the same normalized rows. Add the typed `/v1/org/<store>/read` route and the \
         `CompanyDb::<store>_publish` write authority before adding a store to \
         `BOOT_ADOPTABLE_STORES`.",
        unresolved.join("\n")
    );

    let router = fs::read_to_string(crates_root().join("chiefd-api/src/docstore/router.rs"))
        .expect("chiefd-api/src/docstore/router.rs is readable");
    let production = code_lines(router.split("mod tests").next().unwrap_or(""));
    for retired in [
        "/v1/docs/read",
        "/v1/docs/insert-if-absent",
        "/v1/docs/cas",
        "/v1/docs/drop-company",
        "/v1/docs/drop-company-store",
        "/v1/docs/prune-prefix",
        "/v1/docs/export-all",
        "/v1/docs/list-stores",
    ] {
        assert!(
            !production.contains(&format!(".route(\"{retired}\"")),
            "retired raw document authority route `{retired}` was reintroduced; boot-seeded \
             stores must remain typed-row-only"
        );
    }
}

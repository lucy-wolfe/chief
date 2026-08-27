//! The `chiefd run` adapter over chiefd-core's normalized `org.sqlite` facts.
//!
//! This module opens the company database once per pass, derives its canonical
//! composite namespace key, and translates a failure into one bounded,
//! log-safe `String` the trait adapters
//! in [`super::cycle_input`] / [`super::health_snapshot`] fold into a
//! [`chiefd_core::runtime::duty_hooks::DutyError`].
//!
//! # Why a fresh connection per pass, not a held one
//!
//! `Arc<dyn CycleInputGatherer>` / `Arc<dyn HealthSnapshotGatherer>` are held
//! for the daemon's whole lifetime and called from independent duty tasks
//! (plan: one task per duty). A `rusqlite::Connection` is not `Sync`, so
//! holding one across calls would need a mutex serializing two duties that
//! have no reason to wait on each other. Opening read-only per pass avoids
//! that entirely, and it is cheap: no schema is applied, no write lock is
//! taken, and the file is local — `open_company_db_readonly`'s own docs cover
//! why a read-only connection can never contend with a live writer.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chiefd_core::store::health_collect::{RuntimeDocObservation, SupervisorObservation};
use chiefd_core::store::reconciler_facts::{
    read_launch_intent_person_ids, read_open_maintenance_person_ids, read_pending_mail_facts_after,
    read_runtime_document, read_runtime_owner, read_supervisor_liveness, PendingMailFact,
    ReconcilerFactsError,
};
use chiefd_core::store::supervision::IdentityObservation;
use chiefd_core::store::{open_company_db_readonly, StoreId};

/// Where the shared company `org.sqlite` lives, and the data root chiefd was
/// started with — the `documentKey(slug, dataRoot)` namespace half
/// (`org-durable-store.ts:41`).
#[derive(Debug, Clone)]
pub struct ReconcilerFactsStore {
    db_path: PathBuf,
}

fn map_fact_error<T>(result: Result<T, ReconcilerFactsError>, label: &str) -> Result<T, String> {
    result.map_err(|error| format!("{label} read failed: {error}"))
}

// TOMBSTONE (chief-home-is-cwd §4c): `map_sql_error` stood here — the same
// shaping as `map_fact_error` above, for readers that returned a raw
// `rusqlite::Result` rather than a `ReconcilerFactsError`. Its only callers
// were the three CEO-boot-lease readers, deleted with the lease.

impl ReconcilerFactsStore {
    /// Build a reader over `db_path` (the same file `CHIEFD_STORE_DB_PATH`
    /// names for the docstore mount), namespaced by `data_root` exactly as the
    /// TypeScript `documentKey` is.
    #[must_use]
    pub fn new(db_path: PathBuf, _data_root: impl Into<String>) -> Self {
        Self { db_path }
    }

    fn open(&self) -> Result<rusqlite::Connection, String> {
        open_company_db_readonly(&self.db_path).map_err(|error| {
            format!(
                "cannot open the company org.sqlite at {} read-only: {error}",
                self.db_path.display()
            )
        })
    }

    /// The D9 identity/suppression gate: who currently claims the runtime
    /// runtime, and whether a CEO-only boot lease currently holds the company
    /// exclusive. `our_socket` is the runtime socket THIS chiefd daemon actuates
    /// against (`config.runtime_socket`) — a runtime-owner claim naming any other
    /// socket is [`IdentityObservation::Foreign`].
    ///
    /// # Errors
    /// A bounded message when the connection cannot be opened, or when the
    /// runtime-owner row is present but untrustworthy (unparseable, or naming
    /// a different organization) — this is the safety-critical half, so it
    /// fails closed.
    ///
    /// TOMBSTONE (chief-home-is-cwd §4c): this was `identity_and_suppression`
    /// and returned `(IdentityObservation, bool)`, the second half being "is a
    /// CEO-only boot lease held right now". Two more lease readers —
    /// `ceo_boot_lease_is_active` and `active_ceo_boot_lease_observation` —
    /// stood beside it for the apply-time re-check. All three are deleted with
    /// the lease itself: the daemon boots no pane, so nothing ever takes a
    /// lease and every one of the three could only answer "not held".
    pub fn identity_observation(
        &self,
        slug: &str,
        organization: &str,
        our_socket: &str,
    ) -> Result<IdentityObservation, String> {
        let conn = self.open()?;
        let row_slug = slug;
        // A fresh normalized store holds no claim. `Owned` here is the observed
        // state, not a fabricated placement: the typed row has never been
        // written.
        let owner =
            map_fact_error(read_runtime_owner(&conn, row_slug, organization), "runtime-owner")?;
        Ok(match owner.as_ref().and_then(|owner| owner.foreign_to(our_socket)) {
            Some(holder) => IdentityObservation::Foreign { holder: holder.to_string() },
            None => IdentityObservation::Owned,
        })
    }

    /// The runtime socket named by a LIVE runtime-ownership claim, if any (#63/#64).
    ///
    /// The same row `identity_and_suppression` judges against, read for the
    /// opposite purpose: instead of asking "is the socket I already chose
    /// foreign?", ask "which socket does this company actually run on?" so the
    /// daemon can adopt it and never be foreign to its own company by accident.
    /// A released (or absent) claim returns `None` — nobody is running, so
    /// there is nothing to adopt.
    ///
    /// # Errors
    /// A bounded message when the connection cannot be opened or the
    /// runtime-owner row is present but untrustworthy — same fail-closed
    /// contract as `identity_observation`, because the caller uses this to
    /// decide where it will actuate.
    pub fn active_runtime_owner_socket(
        &self,
        slug: &str,
        organization: &str,
    ) -> Result<Option<String>, String> {
        let conn = self.open()?;
        let row_slug = slug;
        let owner =
            map_fact_error(read_runtime_owner(&conn, row_slug, organization), "runtime-owner")?;
        Ok(owner.and_then(|owner| if owner.status == "active" { owner.socket_name } else { None }))
    }

    /// The launch-intent fence's explicit person set, for the converge
    /// cycle's activity-fence projection (`reconcile_cycle`). `organization`
    /// is checked against the row exactly as the TypeScript loader validates
    /// it: a row written for another company is authority from a different
    /// runtime and reads as the empty (CEO-only) fence.
    ///
    /// # Errors
    /// A bounded message when the connection cannot be opened — intent
    /// unobservable. The converge caller fails the pass closed on this rather
    /// than re-projecting activity from a fabricated empty fence, which would
    /// plan kills for every staffed person. Row-level problems (absent,
    /// corrupt, foreign) are NOT errors here: they are the fence's restrictive
    /// value, matching `loadOrganizationLaunchIntentPersonIds`.
    pub fn launch_intent_person_ids(
        &self,
        slug: &str,
        organization: &str,
    ) -> Result<BTreeSet<String>, String> {
        let conn = self.open()?;
        let row_slug = slug;
        // No rows means no fence has ever been written, and the empty set is
        // this store's restrictive value (CEO-only).
        // #109: the label is named THROUGH the registry, not hand-spelled.
        // `fence_containment` forbids any source outside a store's own module
        // from writing that store's documents key as a literal, because the key
        // is the bypass — and a text guard cannot tell a diagnostic label from a
        // lookup. Rather than allowlist this file (which would generalise to
        // every future caller of this shape), the name is taken from the store
        // itself, exactly as #98 did for the boot-adopt loop. Bonus: a renamed
        // store can no longer leave this error message silently stale.
        map_fact_error(
            read_launch_intent_person_ids(&conn, row_slug, organization),
            StoreId::LaunchIntent.name(),
        )
    }

    /// Pending recipients reconstructed directly from normalized mailbox rows
    /// for the converge cycle's activity-fence projection. This demand keeps a
    /// newly staffed and mailed person desired-active (BUG-10).
    ///
    /// # Errors
    /// A bounded message when the connection cannot be opened or the query
    /// fails — demand unobservable at all fails the pass closed, like the
    /// launch-intent read, rather than planning kills from a demand picture
    /// the pass could not see.
    pub fn pending_mail_facts(&self, slug: &str) -> Result<Vec<PendingMailFact>, String> {
        self.pending_mail_facts_after(slug, None)
    }

    /// Pending recipients strictly newer than an optional #363 reset
    /// watermark.
    pub fn pending_mail_facts_after(
        &self,
        slug: &str,
        since_exclusive_ms: Option<i64>,
    ) -> Result<Vec<PendingMailFact>, String> {
        let conn = self.open()?;
        let row_slug = slug;
        // No row means there is no mailbox demand. Note this is the one absent
        // value here that feeds a kill decision (BUG-10), and it is safe because
        // the normalized mailbox itself has no pending recipient to hide.
        map_fact_error(
            read_pending_mail_facts_after(&conn, row_slug, since_exclusive_ms),
            "mailbox-demand",
        )
    }

    /// People with queued, running, or applying session maintenance.
    ///
    /// A failed read fails the activity pass closed. Reading failure as an
    /// empty set could remove the process which must finish the request.
    pub fn open_maintenance_person_ids(&self, slug: &str) -> Result<BTreeSet<String>, String> {
        let conn = self.open()?;
        map_fact_error(read_open_maintenance_person_ids(&conn, slug), "session-maintenance-demand")
    }

    /// Read the normalized CEO-only goal-delivery quiesce watermark.
    ///
    /// An absent, malformed, or unreadable row means no watermark. An invalid
    /// timestamp is likewise not authority to suppress a stale-delivery
    /// incident. This mirrors the retired monitor's fail-open optional read:
    /// losing a narrow suppression must not hide every unrelated health
    /// observation. A complete facts-store failure remains observable through
    /// the required [`Self::health_durable_facts`] read later in the same pass.
    pub fn goal_delivery_quiesced_since(&self, slug: &str) -> Option<i64> {
        let mut conn = self.open().ok()?;
        let tx = conn.transaction().ok()?;
        // ONE COLUMN, so this does not reconstruct a document around it. Going
        // through `reconstruct` would mean supplying a display slug for an
        // `organization` field this reader discards, which means reading the
        // name out of `org_settings` — and a company genesis has not named yet
        // would then have no watermark. Failing open here means NO
        // SUPPRESSION, so that coupling would be invisible and wrong in the
        // unsafe direction.
        let quiesced_at =
            chiefd_core::store::goal_delivery_quiesce_rows::quiesced_at(&tx, slug).ok()?;
        quiesced_at.and_then(|value| chiefd_core::isotime::parse_iso_millis(&value))
    }

    /// The two normalized facts one health-monitor pass needs: the supervisor
    /// liveness sample and the runtime projection. Both
    /// readers are fail-open by design (see `read_supervisor_liveness` /
    /// `read_runtime_document`), so this only errors if the connection itself
    /// cannot be opened — a genuine "we could not observe" that must skip the
    /// whole pass rather than report false absence as fact.
    ///
    /// #825-prereq: the supervisor liveness sample is sourced from
    /// [`read_supervisor_liveness`] — the chiefd-owned (Rust)
    /// `SupervisionReconcile` duty watermark. The legacy TypeScript-written
    /// `supervisor-state` document it replaced is deleted, writer and table.
    /// [`chiefd_core::store::reconciler_facts::SupervisorLiveness::to_observation`]
    /// translates the tri-state result into the same `Option<SupervisorObservation>`
    /// shape `health_collect::collect` already consumes, so this rewire changes
    /// WHERE the observation is produced, not what downstream health-incident
    /// logic does with it.
    ///
    /// # Errors
    /// A bounded message when the connection cannot be opened.
    pub fn health_durable_facts(
        &self,
        slug: &str,
    ) -> Result<(Option<SupervisorObservation>, Option<RuntimeDocObservation>), String> {
        let conn = self.open()?;
        let row_slug = slug;
        // Both readers are already fail-open at the row level, so absent rows
        // report `None`; observability degrades, never safety.
        let liveness =
            map_fact_error(read_supervisor_liveness(&conn, row_slug), "supervisor-liveness")?;
        let supervisor = liveness.to_observation();
        let runtime = map_fact_error(read_runtime_document(&conn, row_slug), "runtime document")?;
        Ok((supervisor, runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiefd_core::store::open_company_db;

    fn store_with(
        seed: impl FnOnce(&rusqlite::Connection),
    ) -> (tempfile::TempDir, ReconcilerFactsStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("org.sqlite");
        let conn = open_company_db(&path).expect("open writable fixture");
        seed(&conn);
        drop(conn);
        let store = ReconcilerFactsStore::new(path, "/data/orgs");
        (dir, store)
    }

    fn write_runtime_owner(conn: &rusqlite::Connection, socket: &str) {
        conn.execute(
            "INSERT INTO runtime_owner(slug, socket, status) \
             VALUES('cobalt', ?1, 'active')",
            [socket],
        )
        .expect("runtime-owner row");
    }

    /// #825-prereq: the supervisor half is sourced from the chiefd-owned
    /// `supervisor_watermarks` row for the `SupervisionReconcile` duty (a
    /// successful cycle). The `supervisor_process_state` document it replaced —
    /// which only TypeScript ever wrote — is deleted.
    fn write_health_rows(conn: &rusqlite::Connection) {
        conn.execute(
            "INSERT INTO supervisor_watermarks(\
             slug,duty,interval_ms,last_success_at,run_count) \
             VALUES('cobalt','supervision_reconcile',30000,?1,1)",
            ["2026-07-20T00:00:00.000Z"],
        )
        .expect("supervisor watermark row");
        conn.execute(
            "INSERT INTO runtime(\
             slug,version,observed_at,socket_name,status) \
             VALUES('cobalt',1,?1,'cobalt-bison','running')",
            ["2026-07-20T00:00:00.000Z"],
        )
        .expect("runtime row");
        conn.execute(
            "INSERT INTO runtime_process_handles(slug,person,process_handle) VALUES('cobalt','alice','%1')",
            [],
        )
        .expect("runtime process handle");
    }

    fn write_launch_intent(conn: &rusqlite::Connection, people: &[&str]) {
        for person in people {
            conn.execute("INSERT INTO launch_intent(slug,person_id) VALUES('cobalt',?1)", [person])
                .expect("launch-intent row");
        }
        conn.execute(
            "INSERT INTO org_events(slug,seq,entity,entity_id,op,at) \
             VALUES('cobalt',1,'launch-intent','fixture','upsert',?1)",
            ["2026-07-22T00:00:00.000Z"],
        )
        .expect("launch-intent event fence");
    }

    fn write_pending_mail(conn: &rusqlite::Connection) {
        conn.execute(
            "INSERT INTO mailbox(\
             slug,envelope_id,id,person,from_person_id,to_person_id,message,urgency,\
             created_at,state,updated_at) \
             VALUES('cobalt','msg-1@alice','msg-1','alice','chief','alice','work','normal',?1,'pending',1)",
            ["2026-07-20T00:00:00.000Z"],
        )
        .expect("mailbox row");
    }

    /// Every row-native fact read treats a fresh normalized schema with no rows
    /// as "nothing has been written", not as an unreadable store.
    ///
    /// Asserted read-by-read rather than "does not panic" so each one names the
    /// value it degrades to — the degrade is only defensible because each is
    /// the OBSERVED state of a store no writer has touched.
    #[test]
    fn every_normalized_fact_read_treats_absent_rows_as_nothing_written() {
        let (_dir, store) = store_with(|_| {});

        assert_eq!(
            store.identity_observation("cobalt", "cobalt", "cobalt-bison").expect("owned"),
            IdentityObservation::Owned,
            "no rows means no claim — nobody is running here"
        );
        assert_eq!(store.active_runtime_owner_socket("cobalt", "cobalt").expect("unclaimed"), None);
        assert!(
            store.launch_intent_person_ids("cobalt", "cobalt").expect("fence").is_empty(),
            "the empty fence is this store's restrictive value (CEO-only)"
        );
        assert!(store.pending_mail_facts("cobalt").expect("demand").is_empty());
        let (supervisor, runtime) = store.health_durable_facts("cobalt").expect("health");
        assert!(supervisor.is_none() && runtime.is_none(), "no rows means no health facts");
    }

    #[test]
    fn a_valid_quiesce_row_reads_as_its_epoch_millis() {
        let (_dir, store) = store_with(|conn| {
            conn.execute_batch(
                "INSERT INTO quiesce(slug, since) \
                 VALUES ('cobalt', '2026-07-15T12:00:00.000Z');",
            )
            .expect("seed quiesce fixture");
        });

        assert_eq!(
            store.goal_delivery_quiesced_since("cobalt"),
            chiefd_core::isotime::parse_iso_millis("2026-07-15T12:00:00.000Z")
        );
    }

    /// Required normalized facts still fail closed when the company store is
    /// unavailable; the optional quiesce watermark deliberately fails open.
    #[test]
    fn required_normalized_fact_reads_fail_closed_while_optional_quiesce_fails_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            ReconcilerFactsStore::new(dir.path().join("nope").join("chief.db"), "/data/orgs");

        store.identity_observation("cobalt", "cobalt", "cobalt-bison").expect_err("closed");
        store.active_runtime_owner_socket("cobalt", "cobalt").expect_err("closed");
        store.launch_intent_person_ids("cobalt", "cobalt").expect_err("closed");
        store.pending_mail_facts("cobalt").expect_err("closed");
        assert_eq!(
            store.goal_delivery_quiesced_since("cobalt"),
            None,
            "an unavailable optional suppression read is never authority to suppress mail"
        );
        store.health_durable_facts("cobalt").expect_err("closed");
    }

    #[test]
    fn an_unreadable_quiesce_row_disables_only_its_optional_mail_suppression() {
        let (_dir, store) = store_with(|conn| {
            conn.execute_batch(
                "DROP TABLE quiesce; \
                 CREATE TABLE quiesce (slug TEXT NOT NULL); \
                 INSERT INTO quiesce(slug) VALUES ('cobalt');",
            )
            .expect("malformed quiesce fixture");
        });

        assert_eq!(
            store.goal_delivery_quiesced_since("cobalt"),
            None,
            "an unreadable optional watermark fails open rather than suppressing stale mail"
        );
        let (supervisor, runtime) = store
            .health_durable_facts("cobalt")
            .expect("the independent health facts remain readable");
        assert!(
            supervisor.is_none() && runtime.is_none(),
            "a malformed optional quiesce row must not abort the rest of the health pass"
        );
    }

    /// #109: the launch-intent read's failure label comes from the REGISTRY,
    /// not from a hand-copied literal.
    ///
    /// `fence_containment` forbids naming a store's documents key outside its
    /// own module, and it is a TEXT guard — it cannot tell a diagnostic label
    /// from a lookup key, so the literal here tripped it. The fix routes the
    /// label through `StoreId::LaunchIntent.name()`, and this test is what
    /// makes that more than a lint dodge: if somebody re-spells the label by
    /// hand, or renames the store without updating a copy, the message stops
    /// matching the registry and this fails.
    ///
    /// Exercised through a normalized table with the wrong schema — the shape
    /// that reaches the label. An absent row is the empty fence and an
    /// unopenable file fails before the label is reached.
    #[test]
    fn the_launch_intent_read_labels_its_failure_with_the_registry_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("org.sqlite");
        let conn = chiefd_core::store::open_company_db(&path).expect("open writable fixture");
        conn.execute_batch(
            "DROP TABLE launch_intent; \
             CREATE TABLE launch_intent (slug TEXT NOT NULL);",
        )
        .expect("ddl");
        drop(conn);
        let store = ReconcilerFactsStore::new(path, "/data/orgs");

        let message = store
            .launch_intent_person_ids("cobalt", "cobalt")
            .expect_err("a malformed table is not evidence that nobody is fenced in");

        assert!(
            message.starts_with(StoreId::LaunchIntent.name()),
            "the failure must name the store the registry names, got: {message}"
        );
        // Belt and braces: pin the value too, so a registry rename that also
        // silently changed the on-disk key would still be visible here.
        assert!(message.starts_with("launch-intent"), "got: {message}");
    }

    #[test]
    fn an_absent_org_sqlite_fails_the_whole_read_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReconcilerFactsStore::new(dir.path().join("absent.sqlite"), "/data/orgs");
        let error = store
            .identity_observation("cobalt", "cobalt", "cobalt-bison")
            .expect_err("an absent file must not silently read as owned");
        assert!(error.contains("cannot open"), "{error}");
    }

    #[test]
    fn no_claim_reads_owned() {
        let (_dir, store) = store_with(|_| {});
        let identity =
            store.identity_observation("cobalt", "cobalt", "cobalt-bison").expect("reads");
        assert_eq!(identity, IdentityObservation::Owned);
    }

    #[test]
    fn a_claim_on_another_socket_reads_foreign() {
        let (_dir, store) = store_with(|conn| write_runtime_owner(conn, "cobalt-bison"));
        let identity =
            store.identity_observation("cobalt", "cobalt", "some-other-socket").expect("reads");
        assert_eq!(identity, IdentityObservation::Foreign { holder: "cobalt-bison".to_string() });
    }

    /// Rows are found by the store's LABEL, never by the manifest's slug.
    ///
    /// This used to be about a shared `org.sqlite` namespaced per
    /// `(slug, data_root)`, where the label was the composite
    /// `<slug>@<rootHash>` and differed from the manifest slug only when an
    /// operator shared one orgs root. A company is labelled by its DIRECTORY
    /// KEY now — twelve hex characters carrying no name — so the two always
    /// differ, and a caller that reaches for the manifest slug finds no row at
    /// all rather than the wrong one occasionally. Reading absence as "nobody
    /// holds this runtime" is what makes that dangerous: it is the answer that
    /// authorizes actuating over somebody else's claim.
    #[test]
    fn a_runtime_owner_claim_under_the_store_label_is_not_mistaken_for_an_absent_claim() {
        // THE one definition, not a lookalike literal: a fixture that spelled
        // its own would keep passing after the real one changed shape.
        let label = host_primitives::rendezvous::company_key(std::path::Path::new("/work/cobalt"));
        assert_ne!(label, "cobalt", "the label must not be the name");
        let (_dir, store) = store_with(|conn| {
            conn.execute(
                "INSERT INTO runtime_owner(slug, socket, status) \
                 VALUES(?1, 'cobalt-bison', 'active')",
                [&label],
            )
            .expect("runtime-owner row under the store label");
        });
        let identity = store
            .identity_observation(&label, "cobalt", "some-other-socket")
            .expect("reads the row under the label it was written with");
        assert_eq!(identity, IdentityObservation::Foreign { holder: "cobalt-bison".to_string() });
    }

    // TOMBSTONE (chief-home-is-cwd §4c): four tests stood here —
    // `a_currently_held_boot_lease_reads_suppressed`,
    // `a_boot_lease_that_expires_mid_flight_releases_the_duty_instead_of_wedging_it`,
    // `a_same_socket_runtime_marker_at_or_after_the_lease_is_projection_ready`
    // and `a_foreign_socket_or_pre_lease_runtime_marker_is_not_projection_ready`.
    // Each pinned how the CEO boot lease suppressed or authorized this
    // daemon's reconcile duty. The duty had exactly one thing to be fenced
    // against — an attended CEO-only boot doing slow pre-converge work outside
    // any transaction — and the daemon no longer boots a pane, so no writer can
    // take a lease and the duty has nothing to stand down for.

    #[test]
    fn health_durable_facts_reads_both_absent_as_none() {
        let (_dir, store) = store_with(|_| {});
        let (supervisor, runtime) = store.health_durable_facts("cobalt").expect("reads");
        assert!(supervisor.is_none());
        assert!(runtime.is_none());
    }

    #[test]
    fn health_durable_facts_decodes_present_rows() {
        let (_dir, store) = store_with(write_health_rows);
        let (supervisor, runtime) = store.health_durable_facts("cobalt").expect("reads");
        assert_eq!(supervisor.expect("present").status, "running");
        assert_eq!(runtime.expect("present").process_person_ids, vec!["alice".to_string()]);
    }

    #[test]
    fn health_durable_facts_surfaces_a_failing_supervisor_with_last_error_set() {
        let (_dir, store) = store_with(|conn| {
            conn.execute(
                "INSERT INTO supervisor_watermarks(\
                 slug,duty,interval_ms,last_success_at,run_count,last_failure_at,\
                 last_failure_kind,last_failure_detail,consecutive_failures) \
                 VALUES('cobalt','supervision_reconcile',30000,'2026-07-20T00:00:00.000Z',3,\
                 '2026-07-20T00:05:00.000Z','reconcile_refused','ledger mutate refused',2)",
                [],
            )
            .expect("failing watermark row");
        });
        let (supervisor, _runtime) = store.health_durable_facts("cobalt").expect("reads");
        let supervisor = supervisor.expect("a failing duty still reports a running-ledger sample");
        assert_eq!(supervisor.status, "running");
        assert_eq!(supervisor.last_error.as_deref(), Some("ledger mutate refused"));
        assert_eq!(
            supervisor.last_heartbeat_at.as_deref(),
            Some("2026-07-20T00:00:00.000Z"),
            "the true last success survives even while a failure is outstanding"
        );
    }

    #[test]
    fn health_durable_facts_never_started_is_none_not_a_synthesized_error() {
        // Same assertion as `health_durable_facts_reads_both_absent_as_none`
        // but named for the #825-prereq contract: a chiefd freshly up, with no
        // `SupervisionReconcile` success or failure recorded yet, must not be
        // reported as either healthy (`Some` with no error) or failing
        // (`Some` with `last_error` set) — it is `None`, exactly like today's
        // absent-row behavior.
        let (_dir, store) = store_with(|_| {});
        let (supervisor, _runtime) = store.health_durable_facts("cobalt").expect("reads");
        assert!(supervisor.is_none());
    }

    #[test]
    fn an_absent_org_sqlite_fails_health_facts_closed_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReconcilerFactsStore::new(dir.path().join("absent.sqlite"), "/data/orgs");
        let error = store
            .health_durable_facts("cobalt")
            .expect_err("an unreadable connection must not report false absence");
        assert!(error.contains("cannot open"), "{error}");
    }

    #[test]
    fn launch_intent_person_ids_reads_the_fenced_set() {
        let (_dir, store) = store_with(|conn| write_launch_intent(conn, &["alice", "bob"]));
        let ids = store.launch_intent_person_ids("cobalt", "cobalt").expect("reads");
        assert_eq!(ids.iter().map(String::as_str).collect::<Vec<_>>(), ["alice", "bob"]);
    }

    /// The ROW KEY is what scopes a fence, and nothing else is.
    ///
    /// Two arms have stood in this test's second half and both were
    /// tautologies. AC6 removed a stale SESSION name — the row model derives
    /// the session from the slug, so the caller's copy could never differ.
    /// What replaced it compared the caller's ORGANIZATION against the
    /// document's, and `reconstruct` stamps that field FROM that same
    /// argument, so it answered its own question too; the arm is gone from
    /// `read_launch_intent_person_ids` and this asserts what actually gates.
    ///
    /// That is not a weakening. Foreign authority is excluded STRUCTURALLY
    /// now: one directory holds one company, one company has one database, and
    /// rows are selected by `row_slug`. A fence stored under another key is
    /// unreachable rather than merely rejected — which is the stronger
    /// property, because it cannot be forgotten by a caller that passes the
    /// wrong second argument.
    #[test]
    fn launch_intent_person_ids_reads_absence_as_empty_and_another_key_as_empty() {
        // No row at all: CEO-only, not an error.
        let (_dir, store) = store_with(|_| {});
        assert!(store
            .launch_intent_person_ids("cobalt", "cobalt")
            .expect("absent row reads as the empty fence")
            .is_empty());
        let (_dir, store) = store_with(|conn| write_launch_intent(conn, &["alice"]));
        assert_eq!(
            store
                .launch_intent_person_ids("cobalt", "cobalt")
                .expect("reads")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["alice".to_owned()],
            "the company's own fence must still be readable"
        );
        assert!(
            store
                .launch_intent_person_ids("some-other-key", "cobalt")
                .expect("another key reads as the empty fence")
                .is_empty(),
            "a fence stored under another company's key authorizes nobody"
        );
    }

    #[test]
    fn an_absent_org_sqlite_fails_launch_intent_closed() {
        // The one fail-closed case: intent unobservable at all must not be
        // flattened into the empty fence (which would let a cycle plan kills
        // for every staffed person) — the caller skips the pass instead.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReconcilerFactsStore::new(dir.path().join("absent.sqlite"), "/data/orgs");
        let error = store
            .launch_intent_person_ids("cobalt", "cobalt")
            .expect_err("an unreadable store must not read as CEO-only");
        assert!(error.contains("cannot open"), "{error}");
    }

    #[test]
    fn pending_mail_facts_read_the_pending_bearers_and_timestamps() {
        let (_dir, store) = store_with(write_pending_mail);
        let facts = store.pending_mail_facts("cobalt").expect("reads");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].person_id, "alice");
        assert_eq!(facts[0].created_at, "2026-07-20T00:00:00.000Z");
    }

    #[test]
    fn an_absent_org_sqlite_fails_mailbox_demand_closed_too() {
        // Same polarity as the launch-intent read: demand unobservable at all
        // must not silently read as "nobody has mail" — the converge caller
        // would plan kills from a demand picture it never saw.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReconcilerFactsStore::new(dir.path().join("absent.sqlite"), "/data/orgs");
        let error = store
            .pending_mail_facts("cobalt")
            .expect_err("an unreadable store must not read as no demand");
        assert!(error.contains("cannot open"), "{error}");
    }
}

#[cfg(test)]
mod owner_socket_tests {
    use super::*;

    /// A fresh normalized CompanyDb with no runtime-owner row is unclaimed,
    /// never unreadable.
    #[test]
    fn a_store_with_no_runtime_owner_row_reads_as_unclaimed_not_unreadable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("chief.db");
        // Open through chiefd_core because only it may create the normalized
        // company schema; leave runtime_owner empty.
        drop(chiefd_core::store::open_company_db(&db).expect("create the store file"));
        let store = ReconcilerFactsStore::new(db, dir.path().to_string_lossy().to_string());

        let socket = store
            .active_runtime_owner_socket("northstar", "northstar")
            .expect("a virgin store is unclaimed, never an error");
        assert_eq!(socket, None, "nobody has ever claimed this company");
    }

    /// The invariant #63/#64 exists for: an unopenable store still fails
    /// closed, because an unreadable claim means we genuinely do
    /// not know where the company runs and must never guess.
    #[test]
    fn an_unopenable_store_still_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ReconcilerFactsStore::new(
            dir.path().join("nope").join("chief.db"),
            "/orgs".to_string(),
        );
        store
            .active_runtime_owner_socket("northstar", "northstar")
            .expect_err("an unopenable store is not evidence that nobody claims the company");
    }
}

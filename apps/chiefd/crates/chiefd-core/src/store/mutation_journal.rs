//! The org-mutation JOURNAL's semantics half — port of the policy in
//! `apps/cli/src/legacy/organization/org-mutation-journal.ts`. Storage is
//! [`crate::store::mutation_journal_rows`] (`MutationRecord`, `MutationJournal`,
//! `reconstruct`, `publish`, and the table-local `seq` that preserves append
//! order); this module owns retention, the fail-closed read filter, and the
//! three verbs a caller composes an atomic org mutation out of.
//!
//! ## Why this exists
//!
//! `launchOrganizationUnit` (and every other multi-write org mutation) is a
//! multi-commit sequence with no durable record that the sequence is in
//! flight. A process SIGKILLed midway leaves a partial, unwanted result that
//! nothing can name, adopt, resume, or clean up — and an identical retry is
//! refused as "already exists" rather than recognized as the same crashed
//! attempt. This journal closes that gap: a per-mutation `in-flight` marker
//! committed BEFORE the sequence's first write, so a later reader can decide
//! — adopt if the desired end state matches, or leave refused if it does not —
//! instead of depending on the crashing process's own cleanup to run.
//!
//! Two invariants carried over unchanged from the TS original:
//!
//! 1. **The fingerprint digests the DESIRED END STATE**, never incidental
//!    args, and is caller-supplied — this module never computes one.
//! 2. **Recovery is by a LATER READER** ([`find_adoptable`]), never by the
//!    crashing writer's own cleanup. A SIGKILL marks nothing, and `in-flight`
//!    IS that crash signal: no sweep, no reconciliation pass is added here.
//!
//! ## What changed in the port
//!
//! There is no callback-taking `withOrgMutation` wrapper. The TS async twin
//! ran host work (`callback`) *inside* the same logical unit as the journal
//! commit; porting that shape into chiefd would mean running arbitrary host
//! work inside a writer transaction, which is forbidden here. Instead the
//! caller composes [`begin`] before its own writes and [`resolve`] after —
//! each a single transaction, exactly like every other row mutation in this
//! crate.

use std::collections::BTreeMap;

use rusqlite::Transaction;

use crate::store::mutation_journal_rows::{self, MutationJournal, MutationRecord};
use crate::ChiefdError;

/// How many `committed` entries to retain. In-flight and abandoned entries
/// are NEVER dropped by retention — they are precisely the records a later
/// reader needs to adopt or expire. A committed entry's fence has already
/// been published, so it is kept only as a short audit tail.
///
/// [`mutation_journal_rows::MUTATION_JOURNAL_COMMITTED_CAP`] carries the same
/// value for parity documentation on the storage side; enforcement lives
/// here, in the semantics layer, never trusted from a client.
pub const MUTATION_JOURNAL_COMMITTED_CAP: usize = 32;

/// The terminal status [`resolve`] transitions an `in-flight` record to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    /// The mutation's sequence completed; the fence it guarded is published.
    Committed,
    /// The mutation's sequence threw before completing.
    Abandoned,
}

impl MutationOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// The entries of a stored (or absent/malformed) journal, fail-closed. An
/// absent journal, a document at any version other than `1`, or a
/// non-`in-flight`/`committed`/`abandoned` status all mean "nothing to
/// adopt" — never a panic and never a fabricated record.
///
/// With rows storage the shape most of `journalEntries` (the TS original)
/// guarded against is structurally enforced already: `status` carries a SQL
/// `CHECK` and `reconstruct` only ever returns `version: 1`. The remaining
/// live guard is non-empty `mutation_id`/`verb`/`fingerprint` — SQLite's
/// `NOT NULL` permits the empty string, so this filter is the one thing
/// standing between a defective row (hand-inserted, migrated, or backfilled
/// from a blob) and a caller silently adopting or resolving against it.
fn journal_entries(current: Option<MutationJournal>) -> Vec<MutationRecord> {
    let Some(current) = current else {
        return Vec::new();
    };
    if current.version != 1 {
        return Vec::new();
    }
    current
        .entries
        .into_iter()
        .filter(|entry| {
            !entry.mutation_id.trim().is_empty()
                && !entry.verb.trim().is_empty()
                && !entry.fingerprint.trim().is_empty()
                && matches!(entry.status.as_str(), "in-flight" | "committed" | "abandoned")
        })
        .collect()
}

/// Retain every in-flight/abandoned entry; keep only the newest
/// [`MUTATION_JOURNAL_COMMITTED_CAP`] committed entries.
///
/// Order is PRESERVED (append order == recency), and only the OLDEST
/// committed entries — the earliest-appearing ones — are dropped.
/// Re-partitioning by status would move the just-committed newest record
/// ahead of older ones and drop the wrong entry; preserving order is what
/// keeps "newest committed" actually newest.
fn apply_retention(entries: Vec<MutationRecord>) -> Vec<MutationRecord> {
    let committed_count = entries.iter().filter(|entry| entry.status == "committed").count();
    let mut to_drop = committed_count.saturating_sub(MUTATION_JOURNAL_COMMITTED_CAP);
    if to_drop == 0 {
        return entries;
    }
    let mut result = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.status == "committed" && to_drop > 0 {
            to_drop -= 1; // drop this oldest committed entry
            continue;
        }
        result.push(entry);
    }
    result
}

fn publish_entries(
    tx: &Transaction<'_>,
    slug: &str,
    entries: Vec<MutationRecord>,
) -> Result<(), ChiefdError> {
    let company = crate::store::org_settings::display_slug(tx, slug)?;
    let doc = MutationJournal {
        version: 1,
        organization: company.clone(),
        entries: apply_retention(entries),
        extra: BTreeMap::new(),
    };
    mutation_journal_rows::publish(tx, slug, &doc)?;
    Ok(())
}

/// Commit `record` (expected `status == "in-flight"`) in one transaction,
/// applying retention. This is the FIRST write of an atomic org mutation —
/// the caller's own sequence of writes runs after this returns, in its own
/// transaction(s), and [`resolve`] closes the record out once that sequence
/// either finishes or throws.
///
/// # Errors
/// Any [`ChiefdError`] the underlying row read/publish produces.
pub fn begin(tx: &Transaction<'_>, slug: &str, record: MutationRecord) -> Result<(), ChiefdError> {
    let current = mutation_journal_rows::reconstruct(
        tx,
        slug,
        &crate::store::org_settings::display_slug(tx, slug)?,
    )?;
    let mut entries = journal_entries(current);
    entries.push(record);
    publish_entries(tx, slug, entries)
}

/// Resolve an in-flight mutation record to a terminal status, replacing it in
/// place with a fresh `updatedAt` derived from `now_ms`. Idempotent: resolving
/// an unknown id, or a record already at `outcome`, writes nothing and
/// returns `false`.
///
/// # Errors
/// Any [`ChiefdError`] the underlying row read/publish produces.
pub fn resolve(
    tx: &Transaction<'_>,
    slug: &str,
    mutation_id: &str,
    outcome: MutationOutcome,
    now_ms: i64,
) -> Result<bool, ChiefdError> {
    let current = mutation_journal_rows::reconstruct(
        tx,
        slug,
        &crate::store::org_settings::display_slug(tx, slug)?,
    )?;
    let mut entries = journal_entries(current);
    let Some(index) = entries.iter().position(|entry| entry.mutation_id == mutation_id) else {
        return Ok(false);
    };
    if entries[index].status == outcome.as_str() {
        return Ok(false);
    }
    entries[index].status = outcome.as_str().to_string();
    entries[index].updated_at = crate::isotime::iso_millis(now_ms);
    publish_entries(tx, slug, entries)?;
    Ok(true)
}

/// The most recent `in-flight` record matching `fingerprint`, if any,
/// EXCLUDING `exclude_mutation_id` — the adoption lookup at a refusal site. A
/// unit whose creation is recorded in-flight by this same logical mutation is
/// prior crashed work and is adopted; a unit with no in-flight record is
/// somebody else's completed work and keeps the refusal.
///
/// Only `in-flight` records adopt: a `committed` record's fence is already
/// published (nothing to resume), and an `abandoned` record was a clean
/// failure the caller already saw.
///
/// `exclude_mutation_id` is load-bearing. [`begin`] commits the caller's OWN
/// in-flight record before the caller's sequence runs, so without this
/// exclusion a caller would always find its own just-written record and
/// adopt every duplicate — including a genuine concurrent duplicate with no
/// prior crash, which must stay refused. Passing the current mutation's own
/// id restricts the match to a PRIOR crashed attempt of the same logical
/// mutation, which is the only thing that legitimately adopts.
///
/// # Errors
/// Any [`ChiefdError`] the underlying row read produces.
pub fn find_adoptable(
    tx: &Transaction<'_>,
    slug: &str,
    fingerprint: &str,
    exclude_mutation_id: Option<&str>,
) -> Result<Option<MutationRecord>, ChiefdError> {
    let current = mutation_journal_rows::reconstruct(
        tx,
        slug,
        &crate::store::org_settings::display_slug(tx, slug)?,
    )?;
    let entries = journal_entries(current);
    Ok(entries.into_iter().rev().find(|entry| {
        entry.status == "in-flight"
            && entry.fingerprint == fingerprint
            && exclude_mutation_id != Some(entry.mutation_id.as_str())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// A store holding one company. `org_settings` is seeded because the
    /// journal's derived `organization` is that company's DISPLAY name, and a
    /// company genesis has not named yet has no name to stamp.
    fn open() -> Connection {
        open_named("acme")
    }

    fn open_named(display_slug: &str) -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("schema");
        conn.execute(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, \
             acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) \
             VALUES('acme', ?1, 900000, 60000, 3, 2)",
            rusqlite::params![display_slug],
        )
        .expect("seed org_settings");
        conn
    }

    fn rec(id: &str, verb: &str, fingerprint: &str, status: &str) -> MutationRecord {
        MutationRecord {
            mutation_id: id.into(),
            verb: verb.into(),
            fingerprint: fingerprint.into(),
            status: status.into(),
            started_at: "2026-07-25T06:00:00.000Z".into(),
            updated_at: "2026-07-25T06:00:00.000Z".into(),
            actor: None,
            extra: BTreeMap::new(),
        }
    }

    fn entries(tx: &Transaction<'_>, slug: &str) -> Vec<MutationRecord> {
        mutation_journal_rows::reconstruct(tx, slug, slug)
            .unwrap()
            .map(|j| j.entries)
            .unwrap_or_default()
    }

    /// Insert a row directly, bypassing `begin`/`publish`, so a defective
    /// shape (empty `mutation_id`) that `mutation_journal_rows` cannot itself
    /// reject (`NOT NULL` still permits `''`) can exercise the fail-closed
    /// filter in [`journal_entries`].
    fn insert_raw(
        tx: &Transaction<'_>,
        slug: &str,
        mutation_id: &str,
        seq: i64,
        fingerprint: &str,
    ) {
        tx.execute(
            "INSERT INTO mutation_journal(slug, mutation_id, seq, verb, fingerprint, status, \
             started_at, updated_at, actor) VALUES(?1,?2,?3,'unit-launch',?4,'in-flight', \
             '2026-07-25T06:00:00.000Z','2026-07-25T06:00:00.000Z',NULL)",
            rusqlite::params![slug, mutation_id, seq, fingerprint],
        )
        .expect("raw row insert");
    }

    #[test]
    fn begin_commits_an_in_flight_record() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        let got = entries(&tx, "acme");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].mutation_id, "m1");
        assert_eq!(got[0].status, "in-flight");
    }

    /// The row key is a directory hash and the display slug is the company's
    /// name; the journal's derived `organization` means the latter.
    #[test]
    fn the_journal_is_stamped_with_the_display_slug_not_the_row_key() {
        let mut conn = open_named("a7-seed");
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        let journal = mutation_journal_rows::reconstruct(&tx, "acme", "a7-seed").unwrap().unwrap();
        assert_eq!(journal.organization, "a7-seed");
    }

    #[test]
    fn begin_preserves_append_order_across_calls() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        begin(&tx, "acme", rec("m2", "transfer", "fp-2", "in-flight")).unwrap();
        begin(&tx, "acme", rec("m3", "reparent", "fp-3", "in-flight")).unwrap();
        let got = entries(&tx, "acme");
        assert_eq!(
            got.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            vec!["m1", "m2", "m3"]
        );
    }

    #[test]
    fn retention_drops_only_the_oldest_committed_entries_in_order() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        // 34 committed entries, appended in order m0..m33.
        for i in 0..34 {
            let id = format!("m{i}");
            begin(&tx, "acme", rec(&id, "unit-launch", "fp", "in-flight")).unwrap();
            resolve(&tx, "acme", &id, MutationOutcome::Committed, 1_784_116_800_000 + i).unwrap();
        }
        let got = entries(&tx, "acme");
        assert_eq!(got.len(), MUTATION_JOURNAL_COMMITTED_CAP);
        // The two oldest (m0, m1) were dropped; order is preserved for the rest.
        assert_eq!(got.first().unwrap().mutation_id, "m2");
        assert_eq!(got.last().unwrap().mutation_id, "m33");
        assert_eq!(
            got.iter().map(|e| e.mutation_id.clone()).collect::<Vec<_>>(),
            (2..34).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn retention_never_drops_in_flight_or_abandoned_entries() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        for i in 0..40 {
            let id = format!("c{i}");
            begin(&tx, "acme", rec(&id, "unit-launch", "fp", "in-flight")).unwrap();
            resolve(&tx, "acme", &id, MutationOutcome::Committed, 1_784_116_800_000 + i).unwrap();
        }
        begin(&tx, "acme", rec("live", "unit-launch", "fp-live", "in-flight")).unwrap();
        begin(&tx, "acme", rec("dead", "unit-launch", "fp-dead", "in-flight")).unwrap();
        resolve(&tx, "acme", "dead", MutationOutcome::Abandoned, 1_784_116_900_000).unwrap();

        let got = entries(&tx, "acme");
        let committed = got.iter().filter(|e| e.status == "committed").count();
        assert_eq!(committed, MUTATION_JOURNAL_COMMITTED_CAP);
        assert!(got.iter().any(|e| e.mutation_id == "live" && e.status == "in-flight"));
        assert!(got.iter().any(|e| e.mutation_id == "dead" && e.status == "abandoned"));
    }

    #[test]
    fn resolve_marks_committed_with_a_fresh_updated_at() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        let changed =
            resolve(&tx, "acme", "m1", MutationOutcome::Committed, 1_784_116_805_000).unwrap();
        assert!(changed);
        let got = entries(&tx, "acme");
        assert_eq!(got[0].status, "committed");
        assert_eq!(got[0].updated_at, crate::isotime::iso_millis(1_784_116_805_000));
    }

    #[test]
    fn resolve_on_an_unknown_mutation_id_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        let changed =
            resolve(&tx, "acme", "does-not-exist", MutationOutcome::Committed, 1).unwrap();
        assert!(!changed);
        assert_eq!(entries(&tx, "acme")[0].status, "in-flight", "unrelated record untouched");
    }

    #[test]
    fn resolve_already_at_that_status_is_a_no_op() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("m1", "unit-launch", "fp-1", "in-flight")).unwrap();
        resolve(&tx, "acme", "m1", MutationOutcome::Committed, 100).unwrap();
        let before = entries(&tx, "acme")[0].updated_at.clone();
        let changed = resolve(&tx, "acme", "m1", MutationOutcome::Committed, 999_999).unwrap();
        assert!(!changed, "already-committed resolve must not write again");
        assert_eq!(
            entries(&tx, "acme")[0].updated_at,
            before,
            "updatedAt is untouched by the no-op"
        );
    }

    #[test]
    fn find_adoptable_returns_the_most_recent_matching_in_flight_record() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("old", "unit-launch", "fp-shared", "in-flight")).unwrap();
        begin(&tx, "acme", rec("newer", "unit-launch", "fp-shared", "in-flight")).unwrap();
        let found = find_adoptable(&tx, "acme", "fp-shared", None).unwrap().unwrap();
        assert_eq!(found.mutation_id, "newer", "reverse scan returns the most recent match");
    }

    #[test]
    fn find_adoptable_excludes_the_callers_own_just_written_record() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("prior-crash", "unit-launch", "fp-shared", "in-flight")).unwrap();
        begin(&tx, "acme", rec("self", "unit-launch", "fp-shared", "in-flight")).unwrap();
        // Without the exclusion, "self" (the caller's own just-committed
        // in-flight record) would always be the most recent match.
        let found = find_adoptable(&tx, "acme", "fp-shared", Some("self")).unwrap().unwrap();
        assert_eq!(found.mutation_id, "prior-crash");
    }

    #[test]
    fn find_adoptable_ignores_committed_and_abandoned_and_mismatched_fingerprints() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("done", "unit-launch", "fp-x", "in-flight")).unwrap();
        resolve(&tx, "acme", "done", MutationOutcome::Committed, 1).unwrap();
        begin(&tx, "acme", rec("gone", "unit-launch", "fp-x", "in-flight")).unwrap();
        resolve(&tx, "acme", "gone", MutationOutcome::Abandoned, 2).unwrap();
        begin(&tx, "acme", rec("other-fp", "unit-launch", "fp-y", "in-flight")).unwrap();
        assert_eq!(find_adoptable(&tx, "acme", "fp-x", None).unwrap(), None);
        assert!(find_adoptable(&tx, "acme", "fp-y", None).unwrap().is_some());
    }

    #[test]
    fn an_absent_journal_adopts_nothing() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        assert_eq!(find_adoptable(&tx, "acme", "fp-1", None).unwrap(), None);
    }

    #[test]
    fn fail_closed_filter_drops_a_row_with_an_empty_mutation_id() {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        insert_raw(&tx, "acme", "", 1, "fp-1");
        insert_raw(&tx, "acme", "good", 2, "fp-1");
        // The empty-id row must never surface through the semantics layer,
        // even though the storage layer alone would have returned it.
        let raw = mutation_journal_rows::reconstruct(&tx, "acme", "acme").unwrap().unwrap();
        assert_eq!(raw.entries.len(), 2, "both rows are present in storage");
        let found = find_adoptable(&tx, "acme", "fp-1", None).unwrap().unwrap();
        assert_eq!(found.mutation_id, "good", "the empty-id row is filtered out");
    }

    #[test]
    fn slug_scoping_isolates_companies() {
        let mut conn = open();
        conn.execute(
            "INSERT INTO org_settings(slug, display_slug, supervision_interval_ms, \
             acknowledgement_timeout_ms, acknowledgement_retry_limit, replacement_limit) \
             VALUES('beta', 'beta', 900000, 60000, 3, 2)",
            [],
        )
        .expect("the second company is named too");
        let tx = conn.transaction().unwrap();
        begin(&tx, "acme", rec("a1", "unit-launch", "fp-shared", "in-flight")).unwrap();
        begin(&tx, "beta", rec("b1", "unit-launch", "fp-shared", "in-flight")).unwrap();
        assert_eq!(
            find_adoptable(&tx, "acme", "fp-shared", None).unwrap().unwrap().mutation_id,
            "a1"
        );
        assert_eq!(
            find_adoptable(&tx, "beta", "fp-shared", None).unwrap().unwrap().mutation_id,
            "b1"
        );
    }
}

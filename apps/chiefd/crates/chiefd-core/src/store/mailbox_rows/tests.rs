//! Round-trip, derived-recipients, #493-disjointness and refusal coverage for
//! the mailbox row port (org-data-normalization P0, N-mailbox).

use super::*;
use rusqlite::Connection;

use crate::store::mailbox::{
    HealthIncidentRef, MailboxEnvelope, MailboxState, TerminalFamily, Urgency,
    MAILBOX_ENVELOPE_SCHEMA_VERSION,
};

const SLUG: &str = "acme";

fn open() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("apply company schema");
    conn
}

fn env(id: &str, recipients: &[&str], to: &str) -> MailboxEnvelope {
    MailboxEnvelope {
        schema_version: MAILBOX_ENVELOPE_SCHEMA_VERSION,
        id: id.to_string(),
        organization: SLUG.to_string(),
        from_person_id: "chief".to_string(),
        to: to.to_string(),
        recipients: recipients.iter().map(|s| s.to_string()).collect(),
        body: format!("body of {id}"),
        urgency: Urgency::Normal,
        reply_to: None,
        health_incident: None,
        created_at: "2026-07-25T00:00:00.000Z".to_string(),
    }
}

fn entry(envelope: MailboxEnvelope, person: &str, state: &str) -> MailboxEntry {
    MailboxEntry {
        envelope,
        person: person.to_string(),
        state: state.to_string(),
        updated_at: 1_700_000_000_000,
        extra: BTreeMap::new(),
    }
}

/// A publish then a reconstruct returns byte-identical entries — including the
/// health present-together group and the 6th `delivered` state.
#[test]
fn round_trip_reconstruct_equals_published() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();

    let mut a = env("supervision-1", &["bob"], "bob");
    a.urgency = Urgency::Interrupt;
    a.reply_to = Some("thread-9".to_string());
    let mut h = env("health-1", &["ops"], "ops");
    h.health_incident = Some(HealthIncidentRef {
        fingerprint: "fp-1".to_string(),
        kind: "stall".to_string(),
        recipient_person_id: "ops".to_string(),
    });

    let snapshot = MailboxSnapshot {
        entries: vec![entry(a, "bob", "delivered"), entry(h, "ops", "accepted")],
    };
    assert!(publish(&tx, SLUG, &snapshot).unwrap() > 0);

    let mut got = reconstruct(&tx, SLUG).unwrap();
    got.entries.sort_by_key(|e| e.envelope_id());
    let mut want = snapshot.entries.clone();
    want.sort_by_key(|e| e.envelope_id());
    assert_eq!(got.entries, want);
    tx.commit().unwrap();
}

/// A broadcast writes one row per recipient; `recipients` is reconstructed as
/// the sorted sibling set — never denormalized into a stored column.
#[test]
fn recipients_derived_as_sorted_sibling_set() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let people = ["carol", "alice", "bob"];
    let recipients_sorted = ["alice", "bob", "carol"];
    let entries = people
        .iter()
        .map(|p| entry(env("bcast-1", &recipients_sorted, "carol"), p, "pending"))
        .collect();
    publish(&tx, SLUG, &MailboxSnapshot { entries }).unwrap();

    let got = reconstruct(&tx, SLUG).unwrap();
    assert_eq!(got.entries.len(), 3);
    for e in &got.entries {
        assert_eq!(
            e.envelope.recipients,
            vec!["alice".to_string(), "bob".to_string(), "carol".to_string()],
            "recipients derived as the sorted sibling set"
        );
        assert_eq!(e.envelope.organization, SLUG, "organization derived from slug");
    }
    tx.commit().unwrap();
}

/// #493 disjointness (Fable #5): `delivered` is the only fence-archive terminal;
/// every other terminal is pane-drain; `pending` is in neither; the two families
/// never overlap.
#[test]
fn delivered_and_pane_drain_terminals_are_disjoint() {
    assert_eq!(MailboxState::Pending.terminal_family(), None);
    assert_eq!(MailboxState::Delivered.terminal_family(), Some(TerminalFamily::FenceArchive));
    for s in [
        MailboxState::Accepted,
        MailboxState::Superseded,
        MailboxState::Rejected,
        MailboxState::Resolved,
    ] {
        assert_eq!(s.terminal_family(), Some(TerminalFamily::PaneDrain));
        assert!(s.is_pane_drained() && !s.is_fence_archived());
    }
    assert!(
        MailboxState::Delivered.is_fence_archived() && !MailboxState::Delivered.is_pane_drained()
    );
}

/// Item D: a publish carrying an unmodeled key is a 422 refusal naming the path,
/// never a silent drop.
#[test]
fn unmodeled_keys_are_refused() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let mut e = entry(env("x-1", &["bob"], "bob"), "bob", "pending");
    e.extra.insert("mystery".to_string(), serde_json::json!(1));
    let err = publish(&tx, SLUG, &MailboxSnapshot { entries: vec![e] }).unwrap_err();
    match err {
        ChiefdError::Refused(r) => {
            assert_eq!(r.code, UNMODELED_KEYS);
            assert!(r.message.contains("mystery"));
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    tx.commit().unwrap();
}

/// An unknown state bucket is a 422 refusal, never written (would hit the CHECK).
#[test]
fn an_unknown_state_is_refused() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let e = entry(env("x-2", &["bob"], "bob"), "bob", "not-a-bucket");
    let err = publish(&tx, SLUG, &MailboxSnapshot { entries: vec![e] }).unwrap_err();
    assert!(matches!(err, ChiefdError::Refused(r) if r.code == MAILBOX_INVALID));
    tx.commit().unwrap();
}

/// A second atomic publish replaces the prior snapshot without a caller fence.
#[test]
fn a_second_atomic_publish_replaces_the_prior_snapshot() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    publish(
        &tx,
        SLUG,
        &MailboxSnapshot { entries: vec![entry(env("m-1", &["bob"], "bob"), "bob", "pending")] },
    )
    .unwrap();
    let seq = publish(&tx, SLUG, &MailboxSnapshot { entries: vec![] }).unwrap();
    assert!(seq > 0, "the replacement emits immutable audit identity");
    assert!(reconstruct(&tx, SLUG).unwrap().entries.is_empty());
    tx.commit().unwrap();
}

/// A removed entry (absent from the next snapshot) is deleted and emits a delete.
#[test]
fn a_removed_entry_is_deleted() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let first = MailboxSnapshot {
        entries: vec![
            entry(env("a", &["bob"], "bob"), "bob", "pending"),
            entry(env("b", &["ann"], "ann"), "ann", "pending"),
        ],
    };
    let seq = publish(&tx, SLUG, &first).unwrap();
    let second =
        MailboxSnapshot { entries: vec![entry(env("a", &["bob"], "bob"), "bob", "pending")] };
    let replacement_seq = publish(&tx, SLUG, &second).unwrap();
    assert!(replacement_seq >= seq);
    let got = reconstruct(&tx, SLUG).unwrap();
    assert_eq!(got.entries.len(), 1);
    assert_eq!(got.entries[0].envelope.id, "a");
    tx.commit().unwrap();
}

/// #5 PERF INVARIANT (norm-n8): a per-person delta touches O(delta) rows and
/// emits O(delta) org_events — NOT O(all history). A one-envelope append into a
/// company with lots of settled history writes exactly ONE mailbox row and ONE
/// event, proving the amplification the shard shape killed does not return.
#[test]
fn delta_append_is_o1_not_whole_company() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    // Seed a busy company: 40 settled envelopes across two persons.
    let mut seed = Vec::new();
    for i in 0..20 {
        seed.push(entry(env(&format!("b-{i}"), &["bob"], "bob"), "bob", "accepted"));
        seed.push(entry(env(&format!("c-{i}"), &["carol"], "carol"), "carol", "accepted"));
    }
    publish(&tx, SLUG, &MailboxSnapshot { entries: seed }).unwrap();
    let events_before: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM org_events WHERE slug = ?1",
            rusqlite::params![SLUG],
            |r| r.get(0),
        )
        .unwrap();
    let rows_before: i64 = tx.query_row("SELECT COUNT(*) FROM mailbox", [], |r| r.get(0)).unwrap();

    // A ONE-envelope append for bob via the delta path.
    let seq = delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("new-1", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000009999",
        // No actor: an actor that names no person row is not judged.
        "",
    )
    .unwrap();

    let events_after: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM org_events WHERE slug = ?1",
            rusqlite::params![SLUG],
            |r| r.get(0),
        )
        .unwrap();
    let rows_after: i64 = tx.query_row("SELECT COUNT(*) FROM mailbox", [], |r| r.get(0)).unwrap();
    assert_eq!(
        events_after - events_before,
        1,
        "a 1-envelope delta emits exactly ONE org_events row"
    );
    assert_eq!(rows_after - rows_before, 1, "and inserts exactly ONE mailbox row");
    assert_eq!(seq, events_after, "delta returns the new max seq");

    // A delete is also O(1): one row gone, one delete event.
    let seq2 = delta(&tx, SLUG, "bob", &[], &["b-0@bob".to_string()], "1700000010000", "").unwrap();
    let events_del: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM org_events WHERE slug = ?1",
            rusqlite::params![SLUG],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events_del - events_after, 1, "a 1-envelope delete emits exactly ONE event");
    assert_eq!(seq2, events_del);
    // carol's rows are untouched by bob's deltas (disjoint persons).
    let carol = reconstruct_person(&tx, SLUG, "carol").unwrap();
    assert_eq!(carol.entries.len(), 20);
    tx.commit().unwrap();
}

/// P0 settle fence: the mailbox append and cancellation of a previously
/// approved automatic park are one SQLite transaction. A runtime projection
/// that begins after this commit can no longer observe a pending envelope and
/// still reap the recipient from a stale idle decision.
#[test]
fn pending_delta_rearms_the_idle_lease_and_cancels_only_an_automatic_park() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, idle_since, active_transition_id, updated_at) \
         VALUES(?1, 'bob', 0, '2026-07-27T00:00:00.000Z', 'idle-park', '2026-07-27T00:00:00.000Z')",
        rusqlite::params![SLUG],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, requested_at) \
         VALUES(?1, 'idle-park', 'bob', 'park', 'ready', NULL, 'Idle auto-park.', '2026-07-27T00:00:00.000Z')",
        rusqlite::params![SLUG],
    )
    .unwrap();

    delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("activity-fence", &["bob"], "bob"), "bob", "pending")],
        &[],
        "2026-07-27T00:01:00.000Z",
        "",
    )
    .unwrap();

    let activity: (i64, Option<String>, Option<String>) = tx
        .query_row(
            "SELECT last_desired_active, idle_since, active_transition_id FROM person_activity \
             WHERE slug=?1 AND person_id='bob'",
            rusqlite::params![SLUG],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(activity, (1, None, None));
    let transition: (String, String) = tx
        .query_row(
            "SELECT status, reason FROM transitions WHERE slug=?1 AND id='idle-park'",
            rusqlite::params![SLUG],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(transition.0, "cancelled");
    assert_eq!(transition.1, "superseded-by-durable-activity");
    tx.commit().unwrap();
}

/// Mail activity must never silently override an explicit operator lifecycle
/// decision. The envelope remains durable for recovery/audit, but an owned
/// stop transition stays exactly as requested.
#[test]
fn pending_delta_never_supersedes_an_explicit_stop() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO person_activity(slug, person_id, last_desired_active, idle_since, active_transition_id, updated_at) \
         VALUES(?1, 'bob', 0, '2026-07-27T00:00:00.000Z', 'operator-stop', '2026-07-27T00:00:00.000Z')",
        rusqlite::params![SLUG],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO transitions(slug, id, person_id, action, status, intent_id, reason, requested_at) \
         VALUES(?1, 'operator-stop', 'bob', 'park', 'ready', 'person-stop:1', 'operator', '2026-07-27T00:00:00.000Z')",
        rusqlite::params![SLUG],
    )
    .unwrap();

    delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("explicit-stop", &["bob"], "bob"), "bob", "pending")],
        &[],
        "2026-07-27T00:01:00.000Z",
        "",
    )
    .unwrap();

    let activity: (i64, Option<String>, Option<String>) = tx
        .query_row(
            "SELECT last_desired_active, idle_since, active_transition_id FROM person_activity \
             WHERE slug=?1 AND person_id='bob'",
            rusqlite::params![SLUG],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        activity,
        (0, Some("2026-07-27T00:00:00.000Z".to_string()), Some("operator-stop".to_string()))
    );
    let status: String = tx
        .query_row(
            "SELECT status FROM transitions WHERE slug=?1 AND id='operator-stop'",
            rusqlite::params![SLUG],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "ready");
    tx.commit().unwrap();
}

/// reconstruct_person returns ONLY that person's rows, with each envelope's
/// `recipients` COMPLETED across sibling rows (a bare person filter would drop
/// co-recipients).
#[test]
fn reconstruct_person_filters_and_completes_recipients() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    let entries = vec![
        entry(env("bcast", &["bob", "carol"], "bob"), "bob", "pending"),
        entry(env("bcast", &["bob", "carol"], "bob"), "carol", "pending"),
        entry(env("solo", &["bob"], "bob"), "bob", "pending"),
    ];
    publish(&tx, SLUG, &MailboxSnapshot { entries }).unwrap();

    let bob = reconstruct_person(&tx, SLUG, "bob").unwrap();
    let mut ids: Vec<String> = bob.entries.iter().map(|e| e.envelope_id()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["bcast@bob".to_string(), "solo@bob".to_string()],
        "bob sees only his rows"
    );
    let bcast = bob.entries.iter().find(|e| e.envelope.id == "bcast").unwrap();
    assert_eq!(
        bcast.envelope.recipients,
        vec!["bob".to_string(), "carol".to_string()],
        "recipients completed across siblings, not just the filtered person"
    );
    tx.commit().unwrap();
}

// --- authorization: a delta is CONSUMPTION or DELIVERY ---------------------
//
// `person` is WHOSE MAILBOX and never WHO IS ASKING, and the product calls this
// route both ways — `personId = recipient` when one person messages another,
// `personId = context.personId` when a pane settles its own queue. So the rule
// cannot be a binding, and every test below states one half of it.

/// The department every seeded person sits in. `people.department_id` carries a
/// real FOREIGN KEY to `departments`, and `COMPANY_SCHEMA_SQL` turns foreign
/// keys ON, so a person row cannot exist without it. Idempotent, because a test
/// seeds several people into the same unit.
fn seed_department(tx: &rusqlite::Transaction<'_>) {
    tx.execute(
        "INSERT OR IGNORE INTO departments(slug, id, parent_id, name, kind, state, \
         head_person_id, ordinal, created_at, updated_at) \
         VALUES(?1, 'eng', NULL, 'Engineering', 'company', 'active', 'chief', 0, \
         '2026-07-25T00:00:00.000Z', '2026-07-25T00:00:00.000Z')",
        rusqlite::params![SLUG],
    )
    .unwrap();
}

/// Give `id` a real `people` row, so the actor rule sees a person rather than
/// free-form audit prose — `actor_names_a_person` is a `people` lookup, so a
/// test that skipped this would prove only that an unknown actor is unjudged.
fn seed_person(tx: &rusqlite::Transaction<'_>, id: &str) {
    seed_department(tx);
    // `people` carries `CREATE UNIQUE INDEX people_ordinal ON people(slug,
    // ordinal)`, so a CONSTANT ordinal collides on the second person — and the
    // rule under test is one person acting on another person's mailbox, so
    // every interesting case seeds at least two. Derived from what is already
    // seeded rather than passed in, so a caller cannot get it wrong.
    let ordinal: i64 = tx
        .query_row("SELECT COUNT(*) FROM people WHERE slug = ?1", rusqlite::params![SLUG], |r| {
            r.get(0)
        })
        .unwrap();
    tx.execute(
        "INSERT INTO people(slug, id, name, title, mandate, kind, employment_state, \
         department_id, ordinal, created_at, updated_at) \
         VALUES(?1, ?2, ?2, 'Engineer', 'work', 'worker', 'active', 'eng', ?3, \
         '2026-07-25T00:00:00.000Z', '2026-07-25T00:00:00.000Z')",
        rusqlite::params![SLUG, id, ordinal],
    )
    .unwrap();
}

fn refusal_code(err: &ChiefdError) -> &str {
    match err {
        ChiefdError::Refused(r) => r.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// POSITIVE #1 — DELIVERY. One person messages another: `person` is the
/// RECIPIENT and the envelope is from the caller. This is exactly what
/// `publishMailboxEnvelope` sends, and a rule that refused it would silence
/// every message in the product.
#[test]
fn a_delivery_from_the_caller_into_another_persons_mailbox_applies() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "chief");
    seed_person(&tx, "bob");

    let seq = delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("hello", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000000000",
        // `env` stamps `from_person_id: "chief"`, so this IS a delivery from the
        // caller.
        "chief",
    )
    .unwrap();
    assert!(seq > 0);
    assert_eq!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries.len(), 1);
    tx.commit().unwrap();
}

/// POSITIVE #2 — CONSUMPTION. A pane settles its OWN queue: `person` is the
/// caller, the envelope came from somebody else, and the row is one the caller
/// already holds. `settleMailboxEntry` and `settleMailboxBatch` are the entire
/// drain path and both look exactly like this — they read the envelope back,
/// change only `state`, and post it again.
#[test]
fn a_caller_may_settle_and_delete_a_row_it_already_holds() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "bob");
    // `ceo` delivered it (here through the snapshot publish, which is the
    // in-process delivery sink's path).
    publish(
        &tx,
        SLUG,
        &MailboxSnapshot { entries: vec![entry(env("hello", &["bob"], "bob"), "bob", "pending")] },
    )
    .unwrap();

    // Settle: an upsert that moves the row into a terminal bucket. The envelope
    // is from `ceo` — holding the row is what authorizes this, not the sender.
    delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("hello", &["bob"], "bob"), "bob", "accepted")],
        &[],
        "1700000000000",
        "bob",
    )
    .unwrap();
    assert_eq!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries[0].state, "accepted");

    // Consume: deleting from your own mailbox is allowed.
    delta(&tx, SLUG, "bob", &[], &["hello@bob".to_string()], "1700000001000", "bob").unwrap();
    assert!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries.is_empty());
    tx.commit().unwrap();
}

/// SELF-FORGERY IS NOT HARMLESS, WHICH IS WHY CASE 2 IS "A ROW YOU HOLD" AND
/// NOT "YOUR OWN MAILBOX".
///
/// A person MINTING a new envelope in their own mailbox attributed to somebody
/// else manufactures evidence of a message that was never sent — and two other
/// readers consume it. `apps/web` forwards every envelope opaque to an
/// operator's browser, which renders `fromPersonId` as the sender; and chiefd's
/// own launch demand branches on it through `is_launcher_re_emission`. So a new
/// row must always be a delivery from the caller, even into its own mailbox.
#[test]
fn a_caller_may_not_mint_a_row_in_its_own_mailbox_attributed_to_somebody_else() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "bob");

    // `env` stamps `from_person_id: "chief"`, and no such row exists yet.
    let err = delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("invented", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000000000",
        "bob",
    )
    .unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_NOT_A_DELIVERY);
    assert!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries.is_empty());
    tx.commit().unwrap();
}

/// SELF-SUPPRESSION, the concrete harm behind the test above. `fromPersonId:
/// "launcher"` plus a `supervision-` id is what
/// `mailbox::is_launcher_re_emission` matches, and
/// `reconciler_facts::read_pending_mail_facts_after` uses it to drop an
/// envelope from launch DEMAND. Minting one in your own mailbox would be a
/// person quietly arranging not to be woken.
#[test]
fn a_caller_cannot_mint_a_launcher_cadence_envelope_in_its_own_mailbox() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "bob");
    let mut cadence = entry(env("supervision-abc", &["bob"], "bob"), "bob", "pending");
    cadence.envelope.from_person_id = "launcher".to_string();
    // The shape chiefd's own demand filter recognises, so the harm is real.
    assert!(crate::store::mailbox::is_launcher_re_emission(&cadence.envelope));

    let err = delta(&tx, SLUG, "bob", &[cadence], &[], "1700000000000", "bob").unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_NOT_A_DELIVERY);
    tx.commit().unwrap();
}

/// The other half of the same rule: a launcher notice chiefd ITSELF wrote is a
/// row the person already holds, so draining it still works. Constraining case 2
/// must cost the drain path nothing.
#[test]
fn a_caller_may_still_settle_a_launcher_notice_chiefd_wrote() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "bob");
    let mut notice = entry(env("supervision-abc", &["bob"], "bob"), "bob", "pending");
    notice.envelope.from_person_id = "launcher".to_string();
    publish(&tx, SLUG, &MailboxSnapshot { entries: vec![notice.clone()] }).unwrap();

    let mut settled = notice;
    settled.state = "accepted".to_string();
    delta(&tx, SLUG, "bob", &[settled], &[], "1700000000000", "bob").unwrap();

    assert_eq!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries[0].state, "accepted");
    tx.commit().unwrap();
}

/// THE FORGERY. A worker upserts into another mailbox an envelope that claims to
/// be from somebody else. This is the quietest attack the mailbox has: the
/// recipient renders `fromPersonId` as the author, so an unbound upsert puts
/// words in another person's mouth inside a third person's inbox.
#[test]
fn an_upsert_into_another_mailbox_that_is_not_from_the_caller_is_refused() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "chief");
    seed_person(&tx, "bob");
    seed_person(&tx, "mallory");

    let err = delta(
        &tx,
        SLUG,
        "bob",
        // `env` stamps `from_person_id: "chief"`, and the caller is `mallory`.
        &[entry(env("forged", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000000000",
        "mallory",
    )
    .unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_NOT_A_DELIVERY);
    // AND NOTHING WAS WRITTEN. A refusal that returned the right code after
    // inserting would satisfy the assertion above.
    assert!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries.is_empty());
    tx.commit().unwrap();
}

/// The refusal must say WHICH ENTRY and WHY, because this is the first route in
/// the sweep where one request carries several separately-judged items — a
/// caller told only "forbidden" cannot tell which of five messages failed.
#[test]
fn the_delivery_refusal_names_the_entry_and_its_sender() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "mallory");
    seed_person(&tx, "bob");

    let err = delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("forged", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000000000",
        "mallory",
    )
    .unwrap_err();

    let ChiefdError::Refused(refusal) = &err else { panic!("expected a refusal: {err:?}") };
    assert!(refusal.message.contains("forged@bob"), "{}", refusal.message);
    assert!(refusal.message.contains("chief"), "{}", refusal.message);
    assert!(refusal.message.contains("mallory"), "{}", refusal.message);
}

/// DELETES ARE CONSUMPTION ONLY. A delete destroys a durable record, and there
/// is no such thing as delivering a deletion — so a caller may delete only from
/// its own mailbox, even when it could legitimately deliver into that same one.
#[test]
fn a_delete_from_another_persons_mailbox_is_refused_even_for_a_valid_sender() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "chief");
    seed_person(&tx, "bob");
    // `ceo` legitimately delivered this envelope a moment ago.
    delta(
        &tx,
        SLUG,
        "bob",
        &[entry(env("hello", &["bob"], "bob"), "bob", "pending")],
        &[],
        "1700000000000",
        "chief",
    )
    .unwrap();

    let err = delta(&tx, SLUG, "bob", &[], &["hello@bob".to_string()], "1700000001000", "chief")
        .unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_FOREIGN_DELETE);
    // The message survives: sending it did not buy the right to unsend it.
    assert_eq!(reconstruct_person(&tx, SLUG, "bob").unwrap().entries.len(), 1);
    tx.commit().unwrap();
}

/// A MIXED BATCH FAILS WHOLE. One delta may carry several entries with
/// different verdicts; the good ones do not land. The delta already runs in ONE
/// `BEGIN IMMEDIATE` and answers a single `seq`, so a partial apply would have
/// to invent a per-entry outcome shape no caller reads — and a silently dropped
/// entry is the failure mode this module refuses everywhere else.
#[test]
fn a_mixed_batch_is_refused_whole_and_writes_nothing() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "chief");
    seed_person(&tx, "mallory");
    seed_person(&tx, "bob");

    let mut good = entry(env("genuine", &["bob"], "bob"), "bob", "pending");
    good.envelope.from_person_id = "mallory".to_string();
    // `env` stamps `ceo`, so the second entry is the forged one.
    let forged = entry(env("forged", &["bob"], "bob"), "bob", "pending");

    let err =
        delta(&tx, SLUG, "bob", &[good, forged], &[], "1700000000000", "mallory").unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_NOT_A_DELIVERY);
    assert!(
        reconstruct_person(&tx, SLUG, "bob").unwrap().entries.is_empty(),
        "the entry that WOULD have been allowed must not land either"
    );
    tx.commit().unwrap();
}

/// A caller may not claim to be the RUNTIME. `fromPersonId: "launcher"` marks a
/// system notice that chiefd's own delivery sink writes in-process; over HTTP it
/// is a person dressing a message as an infrastructure alert.
#[test]
fn a_caller_cannot_deliver_as_the_launcher() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    seed_person(&tx, "mallory");
    seed_person(&tx, "bob");
    let mut launcher = entry(env("system-notice", &["bob"], "bob"), "bob", "pending");
    launcher.envelope.from_person_id = "launcher".to_string();

    let err = delta(&tx, SLUG, "bob", &[launcher], &[], "1700000000000", "mallory").unwrap_err();

    assert_eq!(refusal_code(&err), MAILBOX_DELTA_NOT_A_DELIVERY);
    tx.commit().unwrap();
}

/// THE ACTOR RULE. `operator`, `op` and the empty string all appear as actors in
/// this corpus and name nobody, so gating on the string's CONTENT would need a
/// placeholder allowlist that rots on the first unlisted spelling. Enforcement
/// fires only when the actor NAMES A PERSON ROW — which is why every
/// pre-existing delta test in this file still passes untouched but for the new
/// argument.
#[test]
fn an_actor_that_names_no_person_is_not_judged() {
    for actor in ["operator", "op", "", "launcher"] {
        let mut conn = open();
        let tx = conn.transaction().unwrap();
        seed_person(&tx, "bob");
        // A delivery that would be forged if the actor were a person: unjudged,
        // because the actor identifies nobody.
        delta(
            &tx,
            SLUG,
            "bob",
            &[entry(env("unjudged", &["bob"], "bob"), "bob", "pending")],
            &[],
            "1700000000000",
            actor,
        )
        .unwrap_or_else(|e| panic!("{actor:?} names nobody and must pass through: {e:?}"));
        tx.commit().unwrap();
    }
}

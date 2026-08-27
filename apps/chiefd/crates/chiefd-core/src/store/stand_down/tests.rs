//! The stand-down rule, pinned at the store.

use super::*;
use crate::store::launch_intent_rows;
use crate::store::rows_txn::current_seq;
use rusqlite::Connection;

fn open() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory db");
    conn.execute_batch(crate::schema::COMPANY_SCHEMA_SQL).expect("apply company schema");
    conn
}

/// Put `people` into the launch-intent fence, the way a start does.
fn fence(tx: &Transaction<'_>, people: &[&str]) {
    for person in people {
        launch_intent_rows::insert_person_fence(tx, "acme", person).expect("fence");
    }
}

/// Who the fence names right now.
fn fenced(tx: &Transaction<'_>) -> Vec<String> {
    let mut stmt = tx
        .prepare("SELECT person_id FROM launch_intent WHERE slug='acme' ORDER BY person_id")
        .expect("prepare");
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).expect("query");
    rows.collect::<Result<Vec<_>, _>>().expect("rows")
}

/// A company that is working normally holds no stand-down, and no verb is
/// refused. The absent row is the working state, deliberately — a product that
/// has never heard of a stand-down behaves exactly as it did.
#[test]
fn a_company_with_no_row_is_working_and_refuses_nothing() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    assert_eq!(read(&tx, "acme").unwrap(), None);
    assert!(!is_stood_down(&tx, "acme").unwrap());
    assert!(refuse_while_stood_down(&tx, "acme", "start").is_ok());
}

/// THE DEFECT, first half: standing down empties the fence in the SAME
/// transaction that records the decision.
///
/// A stand-down recorded without the fence emptied is a company that says it is
/// stopped and keeps working. A fence emptied without the record is the
/// incident itself — six people parked, and the next pass's mail putting every
/// one of them straight back.
#[test]
fn standing_down_records_the_decision_and_empties_the_fence_together() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    fence(&tx, &["alex", "rosa", "sam", "maya", "rhea", "carlos"]);
    assert_eq!(fenced(&tx).len(), 6, "precondition: six people are authorized to run");

    stand_down(&tx, "acme", "2026-08-18T10:00:00.000Z", "operator said stop all work").unwrap();

    assert_eq!(
        read(&tx, "acme").unwrap(),
        Some(StandDown {
            since: "2026-08-18T10:00:00.000Z".into(),
            reason: "operator said stop all work".into(),
        })
    );
    assert!(
        fenced(&tx).is_empty(),
        "the fence must be empty: what is left running is exactly the CEO, who is admitted \
         without a row"
    );
}

/// THE DEFECT, second half: while the stand-down stands, nothing may put
/// anybody back into the fence.
///
/// This is the rule the per-person watermark could not state. It is a question
/// about the COMPANY, and it is asked before any verb grants.
#[test]
fn every_granting_verb_is_refused_while_the_company_is_stood_down() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "2026-08-18T10:00:00.000Z", "").unwrap();

    for verb in ["start", "wake", "hire", "the reconcile pass's mail wake"] {
        let refusal = refuse_while_stood_down(&tx, "acme", verb).expect_err("must refuse");
        assert_eq!(refusal.code(), Some(COMPANY_STOOD_DOWN), "{verb}");
        let said = refusal.to_string();
        assert!(said.contains(verb), "the refusal names the verb: {said}");
        assert!(said.contains("chief resume"), "and names the way out: {said}");
        assert!(said.contains("held, not lost"), "and promises the mail survives: {said}");
    }
}

/// The refusal repeats the operator's own words when they gave any, because a
/// person told only "refused" will invent an explanation.
#[test]
fn the_refusal_repeats_the_operators_reason_when_there_is_one() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "t0", "budget review").unwrap();
    let said = refuse_while_stood_down(&tx, "acme", "start").unwrap_err().to_string();
    assert!(said.contains("budget review"), "{said}");
    assert!(said.contains("t0"), "and says when it started: {said}");
}

/// A stand-down with no reason says so cleanly rather than printing empty
/// brackets at the operator.
#[test]
fn a_stand_down_with_no_reason_reads_cleanly() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "t0", "   ").unwrap();
    let said = refuse_while_stood_down(&tx, "acme", "start").unwrap_err().to_string();
    assert!(!said.contains("()"), "{said}");
}

/// Resuming lifts it, and the company grants again from that moment.
///
/// Note what resume does NOT do: it does not re-fence anybody. The people who
/// were stood down come back because their mail is still pending and the
/// ordinary wake grants them — which is the whole reason the mail is held
/// rather than dropped.
#[test]
fn resuming_lifts_the_stand_down_and_re_fences_nobody() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    fence(&tx, &["alex"]);
    stand_down(&tx, "acme", "t0", "").unwrap();

    resume(&tx, "acme", "t1").unwrap();

    assert_eq!(read(&tx, "acme").unwrap(), None);
    assert!(refuse_while_stood_down(&tx, "acme", "start").is_ok(), "verbs grant again");
    assert!(
        fenced(&tx).is_empty(),
        "resume authorizes nobody by itself: the held mail is what brings people back, and it \
         is still there"
    );
}

/// A stand-down never touches a mailbox row. The mail is HELD, and an operator
/// must be able to pause a company without losing what arrived while it was
/// paused.
#[test]
fn a_stand_down_writes_no_mailbox_row() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO mailbox(slug, envelope_id, id, person, from_person_id, to_person_id, \
         message, urgency, created_at, state, updated_at) \
         VALUES('acme','m1@alex','m1','alex','rosa','alex','are you there?','normal', \
         '2026-08-18T09:59:00.000Z','pending',0)",
        [],
    )
    .expect("seed one pending message");

    stand_down(&tx, "acme", "t0", "").unwrap();
    resume(&tx, "acme", "t1").unwrap();

    let state: String = tx
        .query_row("SELECT state FROM mailbox WHERE slug='acme' AND id='m1'", [], |row| row.get(0))
        .expect("the message is still there");
    assert_eq!(state, "pending", "held, not delivered and not dropped");
}

/// Standing down twice keeps the FIRST decision and writes nothing the second
/// time: the operator's decision is the one they made, and a repeated gesture
/// must not read as a fresh event in the feed.
#[test]
fn standing_down_again_keeps_the_original_decision_and_is_writeless() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "t0", "first").unwrap();
    let after_first = current_seq(&tx, "acme").unwrap();

    stand_down(&tx, "acme", "t1", "second").unwrap();

    assert_eq!(read(&tx, "acme").unwrap().unwrap().since, "t0");
    assert_eq!(read(&tx, "acme").unwrap().unwrap().reason, "first");
    assert_eq!(current_seq(&tx, "acme").unwrap(), after_first, "no second event");
}

/// Resuming a company that is not stood down writes nothing.
#[test]
fn resuming_a_working_company_is_writeless() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    resume(&tx, "acme", "t0").unwrap();
    assert_eq!(current_seq(&tx, "acme").unwrap(), 0);
}

/// Both gestures are in the audit feed, so an operator can see when a company
/// was stopped and when it was let go again.
#[test]
fn both_gestures_are_recorded_in_the_event_feed() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "t0", "").unwrap();
    resume(&tx, "acme", "t1").unwrap();

    let mut stmt = tx
        .prepare("SELECT entity, op FROM org_events WHERE slug='acme' ORDER BY seq")
        .expect("prepare");
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(
        rows,
        vec![
            ("stand-down".to_owned(), "upsert".to_owned()),
            ("stand-down".to_owned(), "delete".to_owned()),
        ]
    );
}

/// One company's stand-down is not another's. The row is keyed by slug, and a
/// box runs many companies.
#[test]
fn a_stand_down_stops_one_company_and_not_its_neighbour() {
    let mut conn = open();
    let tx = conn.transaction().unwrap();
    stand_down(&tx, "acme", "t0", "").unwrap();
    assert!(is_stood_down(&tx, "acme").unwrap());
    assert!(!is_stood_down(&tx, "globex").unwrap(), "a neighbour keeps working");
}

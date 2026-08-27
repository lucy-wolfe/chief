//! The launch-intent fence: the sole authority for *who is allowed to run*.
//!
//! It carries the fence contract of the launcher's now-deleted TypeScript
//! launch-intent module, with one deliberate difference described below.
//!
//! # The bug this module is shaped around
//!
//! In the predecessor system the fence was threaded through as an optional
//! value, and **omitting it meant "no fence"** rather than "a fence with
//! nobody in it". Every read path was therefore one dropped key in a spread,
//! one forgotten parameter, or one refactor away from projecting the entire
//! 28-agent fleet back to running. The TS code bought safety with a pair of
//! deliberately-differently-spelled sentinel constants and a grep test keeping
//! each of them in exactly one file (inv c-1).
//!
//! Rust lets the same guarantee be structural instead. [`LaunchIntent`] is a
//! **single-variant** enum:
//!
//! ```text
//! pub enum LaunchIntent { Fenced(BTreeSet<PersonId>) }
//! ```
//!
//! There is no `Unfenced` variant to construct, to default to, or to
//! accidentally match. `Option<LaunchIntent>` would reintroduce the hazard, so
//! [`read`] is total: it returns a `LaunchIntent` for a missing row, an
//! unparseable row and a row belonging to another company alike, and the value
//! it returns in every one of those cases is `Fenced(∅)` — the CEO and nobody
//! else. Absence is the *safe* case, by type.
//!
//! Polarity: `FailSafeValue` on read, write **and** clear — see the registry
//! entry in [`crate::store`] for why clear is not fail-closed here while the
//! suppression clear is.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::Refusal;
use crate::ledger::Ledgers;
use crate::polarity::{decode_fail_safe_value, Decoded, FailSafeValue, StoreKind};
use crate::store::CompanyContext;

/// Who may be projected to a running pane.
///
/// One variant, on purpose. See the module docs: a permissive variant is not
/// merely discouraged, it does not exist, so no code path — including one
/// written by someone who has never read this file — can obtain one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchIntent {
    /// Exactly these people are authorized for this runtime session. An empty
    /// set is the strictest legal fence, not the absence of one.
    ///
    /// It read "non-CEO people" until #1148 deleted the root's unconditional
    /// lease. The root is nameable here now, because naming it is the only way
    /// to ask for it; it is still never fenced OUT (see `person_can_run`).
    Fenced(BTreeSet<String>),
}

impl LaunchIntent {
    /// The people the fence names — the root included, when it has been asked
    /// for. This said "CEO excluded", which stopped being true when the root's
    /// start decision became a fence entry.
    #[must_use]
    pub fn person_ids(&self) -> &BTreeSet<String> {
        match self {
            Self::Fenced(ids) => ids,
        }
    }

    /// Deny-all: the value corruption, absence and a foreign company all
    /// resolve to.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::Fenced(BTreeSet::new())
    }
}

/// The durable body, byte-compatible with the TS `launch-intent.json` so the
/// Phase-2 import is a parse rather than a translation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchIntentBody {
    /// Schema version. Only `1` exists; anything else is unreadable bytes.
    pub version: u32,
    /// The company slug this ledger belongs to.
    pub organization: String,
    /// A key historical documents carry that this model no longer stores.
    ///
    /// # Why it is a sink and not a field
    ///
    /// It claimed to be "the runtime session this ledger authorizes for", and
    /// [`parse`] compared it against `ctx.session_name()` under a doc comment
    /// saying that caught "a stale runtime session". It could not. The context's
    /// session name is `organization::company_context`'s
    /// `runtime_session_for_slug(&manifest.slug)` — literally `"org-" + slug`,
    /// derived on every read and stored nowhere — and this field was written
    /// from that same call. The check compared a derived constant against
    /// itself, and `org-<slug>` does not go stale for a company whose slug is
    /// its identity. It was a validator that could only ever pass.
    ///
    /// # Why it is not simply deleted
    ///
    /// This struct carries `deny_unknown_fields` with NO `extra` flatten and no
    /// retired-key escape — unlike its row-side namesake
    /// `launch_intent_rows::LaunchIntent`, which is a different struct and has
    /// both. Every historical ledger on disk carries `sessionName`, and the
    /// row-side reconstruct still emits it. Removing the field outright would
    /// make all of them fail to parse; [`parse`] returns an `Err` for that, which
    /// `decode_fail_safe_value` resolves to [`LaunchIntent::deny_all`] — so a
    /// tidy-up would have FENCED EVERY NON-CEO PERSON IN EVERY EXISTING
    /// COMPANY. Accepting and ignoring the key is what makes the row-side
    /// deletion a safe follow-on rather than an outage.
    ///
    /// `Option<String>` rather than `Option<serde_json::Value>`: `Value` does
    /// not implement `Eq`, and this struct derives it.
    #[serde(default, rename = "sessionName", skip_serializing)]
    pub retired_session_name: Option<String>,
    /// Explicit people authorized to run in this runtime session, the root
    /// included when something has asked for it. It read "non-CEO people" while
    /// the root ran on an unconditional lease instead of a decision.
    pub person_ids: Vec<String>,
    /// ISO-8601 timestamp of the last write.
    pub updated_at: String,
    /// Per-person start attributions. The row-side reconstruct
    /// (`launch_intent_rows::LaunchIntent`) always emits this key, and the TS
    /// writer round-trips it through `{...current}`, so a live document may
    /// carry it and the canonical rendering always does. The fence read
    /// consumes only `person_ids`, but refusing the key here would read a
    /// valid attributed fence as deny-all corruption.
    #[serde(default)]
    pub attributions: BTreeMap<String, serde_json::Value>,
}

/// The launch-intent store.
pub struct LaunchIntentStore;

impl StoreKind for LaunchIntentStore {
    const NAME: &'static str = "launch-intent";
    type Body = LaunchIntent;
}

impl FailSafeValue for LaunchIntentStore {
    fn restrictive() -> Self::Body {
        LaunchIntent::deny_all()
    }
}

/// Parse a stored body into the person set it authorizes, returning `None` for
/// anything that is not a valid ledger *for this company*.
///
/// A ledger naming another slug or a stale runtime session is not "slightly
/// wrong"; it is authority from a different runtime, and it takes the same
/// path as garbage bytes.
///
/// Note the return type: the *set*, not the [`LaunchIntent`]. An `Err` here
/// means "did not parse", and letting a bodyless value sit next to a
/// [`LaunchIntent`] in one type — even privately, even briefly — is how "no
/// value" starts standing in for "no fence". Only [`decode_fail_safe_value`]
/// gets to make that conversion, and it can only make it in the restrictive
/// direction.
///
/// Each refusal names itself. This fence DENIES on a refusal, and "everyone is
/// fenced out" is an operator emergency; "which of the five things was wrong
/// with the ledger" is the first question asked and, until now, the warning
/// could not answer it.
fn parse(
    ctx: &CompanyContext,
    body: &str,
) -> Result<BTreeSet<String>, crate::polarity::DecodeRefusal> {
    let stored: LaunchIntentBody =
        serde_json::from_str(body).map_err(|error| format!("the body did not decode: {error}"))?;
    // No session-name comparison. It was a constant against the same constant;
    // see `retired_session_name`. The slug check is the real one — a ledger
    // naming another company IS authority from somewhere else — and it survives
    // untouched.
    if stored.version != 1 {
        return Err(format!("the body is version {}, not 1", stored.version));
    }
    if stored.organization != ctx.slug() {
        return Err(format!(
            "the ledger names organization '{}', not '{}'",
            stored.organization,
            ctx.slug()
        ));
    }
    if stored.updated_at.trim().is_empty() {
        return Err("the ledger carries no updatedAt stamp".to_string());
    }
    // Defensive on read as well as on write: unknown ids are dropped, so a
    // manifest change cannot leave a departed person authorized.
    //
    // TOMBSTONE: `id != ctx.chief_person_id() &&` — the read half of "the CEO is
    // implicitly intended and is never stored in the ledger". It was true while
    // the root had an unconditional lease and false the moment #1148 deleted
    // it: `prepare_ceo_only` writes the root's start decision into this very
    // document, and dropping it on read meant the decision could be written but
    // never read back. See `CompanyContext::in_manifest_order` for the write
    // half and the full account.
    Ok(stored.person_ids.into_iter().filter(|id| ctx.knows_person(id)).collect())
}

/// Read the fence. **Total** — there is no failure mode that is not a fence.
///
/// Returns the decode outcome so callers that surface `warnings[]` can report
/// which fence they are enforcing and why it says what it says.
///
/// # Absence and corruption are different facts with the SAME value
///
/// A fence nobody has written authorizes nobody, exactly as an unreadable one
/// does — the value below is [`LaunchIntent::deny_all`] on both paths, and
/// this doc is the place to say that it must stay that way. "Absence is not
/// corruption" is a statement about the SENTENCE; it must never be read as a
/// licence to make absence permissive, which would turn a wrong word in a
/// warning into an authorization defect.
///
/// What changes is only what is said. An unwritten fence now reports itself as
/// unwritten rather than as unreadable, so an operator asking "why is everybody
/// fenced out" is told the truth — nothing has authorized anyone yet — instead
/// of being sent to look for damaged bytes that do not exist.
#[must_use]
pub fn read(ledgers: &Ledgers, ctx: &CompanyContext) -> Decoded<LaunchIntent> {
    let Some(body) = ledgers.document_body(LaunchIntentStore::NAME) else {
        return Decoded::Absent {
            // Absence value: DENY-ALL, unchanged, and load-bearing.
            body: LaunchIntent::deny_all(),
            // ... and this store is the one absence is worth words about,
            // because the value it produces is a refusal.
            note: Some(format!(
                "store {} has no row; no launch fence has been written, so it authorizes nobody",
                LaunchIntentStore::NAME
            )),
        };
    };
    decode_fail_safe_value::<LaunchIntentStore>(parse(ctx, body).map(LaunchIntent::Fenced))
}

/// The single definition of "may this person run".
///
/// The root CEO always may — it is the durable control plane and is never
/// stored in the ledger. Everyone else may only when the fence names them.
/// Deliberately not a function of department status, fleet suppression (an
/// operator projection) or the manifest: keeping one definition is what stops
/// "who gets a pane" and "who can complete a reflection handoff" from drifting
/// apart.
#[must_use]
pub fn person_can_run(ctx: &CompanyContext, person_id: &str, intent: &LaunchIntent) -> bool {
    person_id == ctx.chief_person_id() || intent.person_ids().contains(person_id)
}

/// Union `person_ids` into the fence — the ONLY way a non-CEO pane becomes
/// launchable, reached exclusively from the explicit operator/CEO launch path.
///
/// # `current` is a PARAMETER, and this is the whole of a live defect
///
/// It read the fence with `read(ledgers, ctx)` — the actor's IN-MEMORY document
/// — and that is not what the fence is. `Ledgers` is hydrated from SQLite once,
/// in `CompanyDb::open`, and thereafter changes only when somebody calls
/// `put_document`. Every per-person grant in the product is a ROW write:
/// `wake_person`, `start_person` and `prepare_ceo_only` all compose
/// [`launch_intent_rows::insert_person_fence`](crate::store::launch_intent_rows::insert_person_fence)
/// inside their own transaction and never touch the document. So the document
/// this used to read was a fence from an older instant, usually the daemon's
/// own start.
///
/// A widening computed from a stale read is not merely late. This commits
/// through `put_document`, and persist-dispatch mirrors the WHOLE document to
/// the rows through
/// [`launch_intent_rows::publish`](crate::store::launch_intent_rows::publish),
/// which set-differences the incoming ids against the committed rows and
/// DELETES every row the document omits — under `apply_and_emit`'s empty actor,
/// with no withdrawal note anywhere. **A grant silently withdrew somebody
/// else's.**
///
/// Measured on `taperoom-inc` (a live box), 2026-08-20:
/// `research-promoter` was woken at `20:34:00.543Z` (`launch-intent upsert`,
/// actor `service`) and her row was deleted at `20:34:02.708Z` with actor `''`,
/// 2.165s later, in a batch of 37 `person-activity` upserts and exactly one
/// `launch-intent` delete — the republish signature. The pass that deleted it
/// still reported `launching: ..., research-promoter, ...`, because the in-memory
/// fence the pass enforced DID name her. She never came up. Of 597 launch-intent
/// deletes that day, 310 had no matching withdrawal line.
///
/// [`remove`] was given this exact parameter for this exact reason and says so
/// in its own doc. `add` narrows too — through `publish`'s set difference — so
/// it takes it as well: `current` is the caller's authoritative read of the
/// committed ROWS right now, and the union is computed over that.
///
/// # Errors
/// [`Refusal`] `launch-intent-unknown-person` when a named person is not in the
/// current manifest. Naming a stranger is a caller bug, and silently dropping
/// them would make `org.department.launch` report a launch it never fenced.
pub fn add(
    ledgers: &mut Ledgers,
    ctx: &CompanyContext,
    current: &LaunchIntent,
    person_ids: impl IntoIterator<Item = String>,
) -> Result<LaunchIntent, Refusal> {
    let requested: Vec<String> = person_ids.into_iter().collect();
    for person_id in &requested {
        if !ctx.knows_person(person_id) {
            return Err(Refusal::new(
                "launch-intent-unknown-person",
                format!("launch intent names unknown person '{person_id}'"),
            )
            .with_routes(vec![
                "org.roster to list the people this organization knows".to_string()
            ]));
        }
    }

    // The caller's fresh read of the committed ROWS, never `read(ledgers, ctx)`.
    // See the doc above: the document lags every row-level grant, and this
    // function's commit path deletes whatever the document omits.
    let mut merged = current.person_ids().clone();
    merged.extend(requested);
    let ordered = ctx.in_manifest_order(&merged);

    let body = LaunchIntentBody {
        version: 1,
        organization: ctx.slug().to_string(),
        // Never written. The sink exists to ACCEPT the key from historical
        // documents, not to keep minting it.
        retired_session_name: None,
        person_ids: ordered.clone(),
        updated_at: ledgers.now().to_iso8601(),
        attributions: BTreeMap::new(),
    };
    let encoded = serde_json::to_string(&body).map_err(|error| {
        Refusal::new(
            "launch-intent-unserializable",
            format!("cannot encode launch intent: {error}"),
        )
    })?;
    ledgers.put_document(LaunchIntentStore::NAME, encoded);
    Ok(LaunchIntent::Fenced(ordered.into_iter().collect()))
}

/// Narrow the fence, withdrawing the named people — the ONLY per-person
/// de-authorization, the shrink half of THE HARD RULE.
///
/// `current` is the caller's authoritative read of the fence *right now*. It
/// is a parameter, not an internal [`read`], for the same reason the converge
/// cycle re-reads the `launch_intent` rows every pass: the actor's in-memory
/// document can lag row-level writes (the public publish route, a launcher
/// grant from another process), and a narrowing computed from a stale read
/// would silently resurrect people a fresher write already removed. The
/// settle path passes the exact fence it just enforced, so withdrawal can
/// only ever subtract from what was genuinely authorized this pass.
///
/// Mirrors [`add`]'s commit semantics in the narrowing direction: the
/// remainder is written through the same single-writer `put_document` path
/// (persist-dispatch mirrors it to the `launch_intent` rows in the same
/// transaction, so the next converge pass reads the narrowed fence).
/// Withdrawal only ever makes the fence stricter, so — like [`clear`] — it
/// never refuses: naming the CEO, a stranger, or a person who was never
/// fenced is a no-op, and a withdraw set that overlaps nothing writes nothing
/// (an idle company stays writeless, #367).
///
/// The committed removal is the *record*; the pane kill derives from it on the
/// pass that observes the narrowed fence — never the other way around.
///
/// # Errors
/// [`Refusal`] `launch-intent-unserializable` when the narrowed body cannot be
/// encoded — the same theoretical failure [`add`] carries, never a policy gate.
pub fn remove(
    ledgers: &mut Ledgers,
    ctx: &CompanyContext,
    current: &LaunchIntent,
    person_ids: impl IntoIterator<Item = String>,
) -> Result<LaunchIntent, Refusal> {
    let requested: std::collections::BTreeSet<String> = person_ids.into_iter().collect();
    let remaining: std::collections::BTreeSet<String> =
        current.person_ids().iter().filter(|id| !requested.contains(*id)).cloned().collect();
    if remaining.len() == current.person_ids().len() {
        // No overlap: the fence is unchanged, so commit nothing. Narrowing can
        // never open the fleet, and an unchanged fence must stay writeless.
        return Ok(current.clone());
    }
    let ordered = ctx.in_manifest_order(&remaining);
    let body = LaunchIntentBody {
        version: 1,
        organization: ctx.slug().to_string(),
        // Never written. The sink exists to ACCEPT the key from historical
        // documents, not to keep minting it.
        retired_session_name: None,
        person_ids: ordered.clone(),
        updated_at: ledgers.now().to_iso8601(),
        attributions: BTreeMap::new(),
    };
    let encoded = serde_json::to_string(&body).map_err(|error| {
        Refusal::new(
            "launch-intent-unserializable",
            format!("cannot encode launch intent: {error}"),
        )
    })?;
    ledgers.put_document(LaunchIntentStore::NAME, encoded);
    Ok(LaunchIntent::Fenced(ordered.into_iter().collect()))
}

/// Reset the fence to CEO-only. Returns whether a row was present.
///
/// `company.boot` and CEO-only recovery call this so a restart from a
/// fully-active state can never bring the fleet back. `company.resume`
/// deliberately does **not** (plan §2.1, flag a-4): that asymmetry is
/// load-bearing and is asserted by
/// `clearing_is_the_restrictive_value_so_it_never_refuses`.
pub fn clear(ledgers: &mut Ledgers) -> bool {
    ledgers.remove_document(LaunchIntentStore::NAME)
}

/// The people a launch intent still authorizes who no longer have any reason
/// to run.
///
/// A person is stale when the activity projection says they should be down AND
/// they owe no transition. The second half is what makes this safe to act on:
/// a person mid-handoff is *also* "not active" in the projection, and
/// withdrawing their intent while their transition is open would tear the pane
/// down underneath the handoff that transition exists to collect.
///
/// Port of `staleLaunchIntentPersonIds` (`org-activity.ts`, deleted).
#[must_use]
pub fn stale_launch_intent_person_ids(
    launch_intent_person_ids: &[String],
    snapshot: &crate::store::activity::ActivitySnapshot,
) -> Vec<String> {
    launch_intent_person_ids
        .iter()
        .filter(|person_id| {
            snapshot
                .people
                .get(*person_id)
                .is_some_and(|decision| !decision.active && decision.transition_id.is_none())
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    /// `add`, with the caller's authoritative read supplied from this ledger.
    ///
    /// Production callers read the committed ROWS (see `add`'s doc: the
    /// document lags every row-level grant, and unioning onto it silently
    /// withdrew an operator's wake). These cases are about union, ordering and
    /// the root's own start decision, and for them the ledger IS the whole
    /// world — so they read it back the same way, once, here, instead of
    /// spelling the two-line read at every call.
    fn add_from_ledger(
        ledgers: &mut Ledgers,
        ctx: &CompanyContext,
        person_ids: impl IntoIterator<Item = String>,
    ) -> Result<LaunchIntent, Refusal> {
        let (current, _) = super::read(ledgers, ctx).into_parts();
        super::add(ledgers, ctx, &current, person_ids)
    }

    /// A person with no decision at all is NOT stale: absence from the
    /// projection means the reconcile has not spoken about them, which is not
    /// the same as speaking and saying "down".
    #[test]
    fn only_a_down_person_with_no_open_transition_is_stale() {
        use crate::store::activity::{ActivityReason, ActivitySnapshot, PersonActivityDecision};
        use std::collections::BTreeMap;

        fn decision(active: bool, transition: Option<&str>) -> PersonActivityDecision {
            PersonActivityDecision {
                person_id: String::new(),
                active,
                reasons: Vec::<ActivityReason>::new(),
                transition_id: transition.map(ToString::to_string),
            }
        }

        let mut people = BTreeMap::new();
        people.insert("down".to_string(), decision(false, None));
        people.insert("up".to_string(), decision(true, None));
        people.insert("handing-off".to_string(), decision(false, Some("t1")));
        let snapshot = ActivitySnapshot { people };

        let authorized = [
            "down".to_string(),
            "up".to_string(),
            "handing-off".to_string(),
            "unknown".to_string(),
        ];
        assert_eq!(
            stale_launch_intent_person_ids(&authorized, &snapshot),
            vec!["down".to_string()]
        );
    }

    use super::*;
    use crate::clock::WallMillis;

    fn ctx() -> CompanyContext {
        CompanyContext::new(
            "cobalt",
            "chief",
            ["chief", "quant-head", "signal-researcher"].map(String::from),
        )
    }

    fn ledgers() -> Ledgers {
        Ledgers::empty(WallMillis(1_752_000_000_000))
    }

    #[test]
    fn an_absent_ledger_fences_everyone_but_the_ceo() {
        let (intent, warning) = read(&ledgers(), &ctx()).into_parts();
        assert_eq!(intent, LaunchIntent::deny_all());
        assert!(warning.is_some(), "a fence conjured from absence is worth a warning");
        assert!(person_can_run(&ctx(), "chief", &intent));
        assert!(!person_can_run(&ctx(), "quant-head", &intent));
    }

    #[test]
    fn unparseable_bytes_deny_rather_than_open() {
        let mut l = ledgers();
        l.put_document(LaunchIntentStore::NAME, "{\"nonsense\":true}");
        let (intent, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(intent, LaunchIntent::deny_all());
        assert!(warning.is_some_and(|w| w.contains("restrictive")));
    }

    #[test]
    fn a_reconstructed_document_carrying_attributions_still_reads_as_its_fence() {
        // The row-side reconstruct (`launch_intent_rows::LaunchIntent`) always
        // emits `attributions`, and the TS writer round-trips the key through
        // `{...current}`. The durable-body parse must not read that modeled
        // key as corruption — before it was accepted, a live attributed fence
        // decoded as deny-all (supervisor_handoff_byte_identity regression).
        let mut l = ledgers();
        l.put_document(
            LaunchIntentStore::NAME,
            "{\"version\":1,\"organization\":\"cobalt\",\"sessionName\":\"cobalt-session\",\
             \"personIds\":[\"quant-head\"],\"updatedAt\":\"2026-07-19T00:00:00.000Z\",\
             \"attributions\":{\"quant-head\":{\"reason\":\"operator start\",\
             \"startedAt\":\"2026-07-19T00:00:00.000Z\"}}}",
        );
        let (intent, warning) = read(&l, &ctx()).into_parts();
        assert!(warning.is_none(), "a valid attributed fence is not corruption: {warning:?}");
        assert!(person_can_run(&ctx(), "quant-head", &intent));
    }

    #[test]
    fn a_ledger_from_another_company_authorizes_nobody() {
        // The surviving half of what used to be one loop over company AND
        // session. The company check is real authority — a ledger naming
        // another slug is a fence from somewhere else — and it is unchanged.
        let mut l = ledgers();
        let body = LaunchIntentBody {
            version: 1,
            organization: "other".to_string(),
            retired_session_name: None,
            person_ids: vec!["quant-head".to_string()],
            updated_at: "2026-07-19T00:00:00.000Z".to_string(),
            attributions: BTreeMap::new(),
        };
        l.put_document(LaunchIntentStore::NAME, serde_json::to_string(&body).expect("encodes"));
        let (intent, _) = read(&l, &ctx()).into_parts();
        assert_eq!(
            intent,
            LaunchIntent::deny_all(),
            "a fence written for another company must not authorize here"
        );
    }

    #[test]
    fn a_historical_ledger_carrying_session_name_still_authorizes_its_people() {
        // THE HAZARD, pinned. Every ledger ever written carries `sessionName`,
        // and the row-side reconstruct still emits it. `LaunchIntentBody` is
        // `deny_unknown_fields` with no `extra` flatten, so deleting the field
        // outright would make all of them fail to parse — and a parse failure
        // here is not a warning, it is `deny_all()`, which fences every
        // non-CEO person in every existing company. The sink is what makes
        // that a non-event.
        let mut l = ledgers();
        l.put_document(
            LaunchIntentStore::NAME,
            "{\"version\":1,\"organization\":\"cobalt\",\"sessionName\":\"cobalt-session\",\
             \"personIds\":[\"quant-head\"],\"updatedAt\":\"2026-07-19T00:00:00.000Z\"}",
        );
        let (intent, warning) = read(&l, &ctx()).into_parts();
        assert!(warning.is_none(), "a historical ledger is not corruption: {warning:?}");
        assert!(
            person_can_run(&ctx(), "quant-head", &intent),
            "the sink must keep a historical fence readable — without it this is deny-all"
        );
    }

    #[test]
    fn the_session_name_no_longer_participates_in_the_fence() {
        // A DELIBERATE behaviour change, recorded rather than dropped. The old
        // check compared `stored.session_name` against `ctx.session_name()`,
        // under a doc comment claiming it caught "a stale runtime session".
        // Both sides were `runtime_session_for_slug(slug)` — `"org-" + slug`,
        // derived on read and stored nowhere — so for every document this
        // system writes it compared a constant with itself. A hand-written
        // session name could trip it, which is the only reason it ever looked
        // alive; nothing in the product produces one, and `org-<slug>` cannot
        // go stale for a company whose slug IS its identity.
        let mut l = ledgers();
        l.put_document(
            LaunchIntentStore::NAME,
            "{\"version\":1,\"organization\":\"cobalt\",\"sessionName\":\"whatever-else\",\
             \"personIds\":[\"quant-head\"],\"updatedAt\":\"2026-07-19T00:00:00.000Z\"}",
        );
        let (intent, _) = read(&l, &ctx()).into_parts();
        assert!(
            person_can_run(&ctx(), "quant-head", &intent),
            "the company check is the authority; the session name is not consulted"
        );
    }

    #[test]
    fn the_written_body_no_longer_mints_a_session_name() {
        // The other direction: the sink ACCEPTS the key, it does not keep
        // producing it. A field that were merely renamed would still write one.
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");
        let written = l.document_body(LaunchIntentStore::NAME).expect("written").to_owned();
        assert!(
            !written.contains("sessionName"),
            "a new fence must not carry the retired key: {written}"
        );
    }

    /// WHAT THIS USED TO CLAIM: `add_authorizes_exactly_the_named_people_and_
    /// never_the_ceo` asserted that adding the root to a fence stored
    /// `["quant-head"]` — the root was silently dropped, because it was
    /// "implicitly intended" by an unconditional lease and never stored.
    ///
    /// #1148 deleted the lease. `prepare_ceo_only` writes the root's start
    /// decision INTO this document, and every `add` re-canonicalizes the whole
    /// fence, so the drop meant the first mail grant to anybody erased the
    /// root's own demand and the company drained to empty. The surviving claim
    /// — a fence authorizes EXACTLY who it names and nobody else — is asserted
    /// unchanged below, and is now true of the root too.
    #[test]
    fn add_authorizes_exactly_the_named_people_including_the_ceo() {
        let mut l = ledgers();
        let intent =
            add_from_ledger(&mut l, &ctx(), ["quant-head".to_string(), "chief".to_string()])
                .expect("accepted");
        assert_eq!(
            intent.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["chief", "quant-head"],
            "the root is stored when it is named -- that row IS its start decision"
        );

        let (reread, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(reread, intent, "the fence survives a round trip through the ledger");
        assert!(warning.is_none(), "a ledger chiefd just wrote parses");
        assert!(person_can_run(&ctx(), "quant-head", &reread));
        assert!(!person_can_run(&ctx(), "signal-researcher", &reread));
    }

    /// THE ROOT'S START DECISION SURVIVES SOMEBODY ELSE'S CHURN.
    ///
    /// The regression this pins, end to end in one store. `prepare_ceo_only`
    /// writes the root into the fence — that entry is the whole of "the
    /// operator asked for the root" since #1148 deleted the unconditional
    /// lease. Both mutating verbs re-canonicalize the ENTIRE fence through
    /// `CompanyContext::in_manifest_order`, which used to evict the CEO, so
    /// granting a worker their mail or withdrawing a settled worker silently
    /// deleted the root's demand as a side effect. The company then drained to
    /// nobody one quiet lease later, for a reason no surface named and that
    /// looked nothing like the write that caused it.
    ///
    /// A round trip through the ledger is part of the claim: the read path
    /// filtered the root out too, so the decision could be written and still
    /// not be there when it was next read.
    #[test]
    fn neither_a_grant_nor_a_withdrawal_evicts_the_roots_own_start_decision() {
        let mut l = ledgers();
        // Genesis/attach: the root is asked for.
        add_from_ledger(&mut l, &ctx(), ["chief".to_string()]).expect("the root's start decision");
        assert_eq!(
            read(&l, &ctx())
                .into_parts()
                .0
                .person_ids()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["chief"],
            "and it is READABLE, not merely written"
        );

        // Somebody else's mail arrives and grants them the fence.
        add_from_ledger(&mut l, &ctx(), ["signal-researcher".to_string()]).expect("a mail grant");
        let (after_grant, _) = read(&l, &ctx()).into_parts();
        assert_eq!(
            after_grant.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["chief", "signal-researcher"],
            "granting a worker must not evict the root"
        );

        // That worker settles and their intent is withdrawn.
        remove(&mut l, &ctx(), &after_grant, ["signal-researcher".to_string()])
            .expect("a settle withdrawal");
        let (after_withdrawal, _) = read(&l, &ctx()).into_parts();
        assert_eq!(
            after_withdrawal.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["chief"],
            "withdrawing a settled worker must not evict the root either -- the fence is \
             CEO-only, and it SAYS so rather than relying on a lease that no longer exists"
        );
    }

    /// A GRANT UNIONS ONTO THE COMMITTED ROWS, NOT ONTO THE LEDGER DOCUMENT.
    ///
    /// The whole of a live defect, in one assertion. `Ledgers` is hydrated from
    /// SQLite once at `CompanyDb::open`; every per-person grant in the product
    /// is a ROW write (`wake_person`, `start_person`, `prepare_ceo_only` all
    /// compose `launch_intent_rows::insert_person_fence`) and never touches the
    /// document. So the document a grant used to union onto was a fence from an
    /// older instant — and because this commits the WHOLE document through
    /// `put_document`, and persist-dispatch mirrors it to the rows by set
    /// difference, the grant DELETED everybody the stale document had not heard
    /// of.
    ///
    /// Measured on `taperoom-inc`, 2026-08-20: `research-promoter` woken at
    /// `20:34:00.543Z`, her row deleted at `20:34:02.708Z` with an empty actor
    /// and no note, by a pass whose own log line said it was launching her.
    ///
    /// The fixture is that shape exactly: the ledger document knows about one
    /// person, the rows know about two, and a grant for a third must not drop
    /// the one the document never saw.
    #[test]
    fn a_grant_never_drops_somebody_the_ledger_document_has_not_heard_of() {
        let mut l = ledgers();
        // What the actor's in-memory document knows: the root only.
        add_from_ledger(&mut l, &ctx(), ["chief".to_string()]).expect("the root's start decision");
        // What the ROWS know, because a wake wrote one directly: the root AND
        // the person the operator just clicked.
        let committed = LaunchIntent::Fenced(
            ["chief".to_string(), "quant-head".to_string()].into_iter().collect(),
        );

        let granted = add(&mut l, &ctx(), &committed, ["signal-researcher".to_string()])
            .expect("a mail grant for a third person");

        assert_eq!(
            granted.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["chief", "quant-head", "signal-researcher"],
            "a grant for one person must not withdraw another: the woken person is in the \
             committed rows and the ledger document has never heard of them, which is the \
             normal state of affairs and not an anomaly"
        );
    }

    #[test]
    fn add_unions_rather_than_replacing_and_stores_manifest_order() {
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["signal-researcher".to_string()]).expect("accepted");
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");

        let stored: LaunchIntentBody =
            serde_json::from_str(l.document_body(LaunchIntentStore::NAME).expect("row"))
                .expect("parses");
        assert_eq!(
            stored.person_ids,
            vec!["quant-head".to_string(), "signal-researcher".to_string()]
        );
    }

    #[test]
    fn add_refuses_a_person_the_manifest_does_not_know() {
        let mut l = ledgers();
        let refusal =
            add_from_ledger(&mut l, &ctx(), ["ghost".to_string()]).expect_err("must refuse");
        assert_eq!(refusal.code, "launch-intent-unknown-person");
        assert!(!refusal.legal_routes.is_empty());
        assert!(
            l.document_body(LaunchIntentStore::NAME).is_none(),
            "a refused add writes no fence at all"
        );
    }

    #[test]
    fn adding_over_a_corrupt_ledger_yields_only_the_newly_named_people() {
        let mut l = ledgers();
        l.put_document(LaunchIntentStore::NAME, "\"not even an object\"");
        let intent = add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");
        assert_eq!(
            intent.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["quant-head"],
            "unreadable bytes contribute nobody; they never contribute everybody"
        );
    }

    fn current(l: &Ledgers) -> LaunchIntent {
        read(l, &ctx()).into_parts().0
    }

    #[test]
    fn remove_withdraws_exactly_the_named_people_and_keeps_the_rest() {
        let mut l = ledgers();
        add_from_ledger(
            &mut l,
            &ctx(),
            ["quant-head".to_string(), "signal-researcher".to_string()],
        )
        .expect("accepted");
        let before = current(&l);
        let intent =
            remove(&mut l, &ctx(), &before, ["signal-researcher".to_string()]).expect("narrows");
        assert_eq!(
            intent.person_ids().iter().map(String::as_str).collect::<Vec<_>>(),
            ["quant-head"]
        );

        let (reread, warning) = read(&l, &ctx()).into_parts();
        assert_eq!(reread, intent, "the narrowed fence survives a round trip through the ledger");
        assert!(warning.is_none(), "a ledger chiefd just wrote parses");
        assert!(person_can_run(&ctx(), "quant-head", &reread));
        assert!(!person_can_run(&ctx(), "signal-researcher", &reread));
        assert!(person_can_run(&ctx(), "chief", &reread), "the CEO is never withdrawn");
    }

    #[test]
    fn remove_of_everything_is_ceo_only_and_remove_of_nothing_writes_nothing() {
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");
        let before = current(&l);
        let intent = remove(&mut l, &ctx(), &before, ["quant-head".to_string()]).expect("narrows");
        assert_eq!(intent, LaunchIntent::deny_all());
        let stored: LaunchIntentBody =
            serde_json::from_str(l.document_body(LaunchIntentStore::NAME).expect("row"))
                .expect("parses");
        assert!(
            stored.person_ids.is_empty(),
            "a fully narrowed fence is the strictest legal value"
        );

        // Idempotent and writeless: withdrawing a person who is not fenced
        // (unknown, the CEO, or already withdrawn) changes nothing and must
        // not touch the durable row — an idle company stays writeless (#367).
        let before = l.document_body(LaunchIntentStore::NAME).expect("row").to_string();
        let current_now = current(&l);
        let unchanged = remove(
            &mut l,
            &ctx(),
            &current_now,
            ["ghost".to_string(), "chief".to_string(), "quant-head".to_string()],
        )
        .expect("narrowing never refuses");
        assert_eq!(unchanged, LaunchIntent::deny_all());
        assert_eq!(
            l.document_body(LaunchIntentStore::NAME),
            Some(before.as_str()),
            "a no-overlap withdrawal commits nothing"
        );

        // An absent ledger withdraws nobody and conjures no row.
        let mut empty = ledgers();
        let absent = current(&empty);
        let intent =
            remove(&mut empty, &ctx(), &absent, ["quant-head".to_string()]).expect("no-op");
        assert_eq!(intent, LaunchIntent::deny_all());
        assert!(empty.document_body(LaunchIntentStore::NAME).is_none());
    }

    #[test]
    fn remove_narrows_the_callers_fresh_read_never_a_stale_cached_document() {
        // The settle path's authority is the fence it just read from the rows,
        // not the actor's in-memory document. A stale cached document must not
        // resurrect a person a fresher row-level write already removed.
        let mut l = ledgers();
        add_from_ledger(
            &mut l,
            &ctx(),
            ["quant-head".to_string(), "signal-researcher".to_string()],
        )
        .expect("accepted");
        let fresh = LaunchIntent::Fenced(["signal-researcher".to_string()].into_iter().collect());
        let intent = remove(&mut l, &ctx(), &fresh, ["signal-researcher".to_string()])
            .expect("narrows the fresh read");
        assert_eq!(intent, LaunchIntent::deny_all());
        let (reread, _) = read(&l, &ctx()).into_parts();
        assert_eq!(
            reread,
            LaunchIntent::deny_all(),
            "quant-head, absent from the caller's fresh read, is not resurrected by the stale cache"
        );
    }

    #[test]
    fn remove_keeps_manifest_order_for_the_remainder() {
        let mut l = ledgers();
        add_from_ledger(
            &mut l,
            &ctx(),
            ["signal-researcher".to_string(), "quant-head".to_string()],
        )
        .expect("accepted");
        let before = current(&l);
        remove(&mut l, &ctx(), &before, ["quant-head".to_string()]).expect("narrows");
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("re-grant");
        let stored: LaunchIntentBody =
            serde_json::from_str(l.document_body(LaunchIntentStore::NAME).expect("row"))
                .expect("parses");
        assert_eq!(
            stored.person_ids,
            vec!["quant-head".to_string(), "signal-researcher".to_string()]
        );
    }

    #[test]
    fn clearing_is_the_restrictive_value_so_it_never_refuses() {
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");
        assert!(clear(&mut l), "a present fence is cleared");
        assert!(!clear(&mut l), "clearing is idempotent");
        let (intent, _) = read(&l, &ctx()).into_parts();
        assert_eq!(intent, LaunchIntent::deny_all());

        // Even a fence nobody can parse clears: the result is CEO-only, which
        // is the direction that cannot open the fleet.
        let mut corrupt = ledgers();
        corrupt.put_document(LaunchIntentStore::NAME, "{}");
        assert!(clear(&mut corrupt));
        assert_eq!(read(&corrupt, &ctx()).into_parts().0, LaunchIntent::deny_all());
    }

    #[test]
    /// RENAMED, because the old name said "departed" and the body tested
    /// REMOVAL. The distinction is the whole hazard: a REMOVED person is gone
    /// from the manifest, so `parse` drops their id on read and the stored
    /// document cannot authorize them. A DEPARTED person is RETAINED in the
    /// manifest by design (durable history and audit), so `knows_person` still
    /// knows them and this defense does not apply to them at all.
    fn a_person_removed_from_the_manifest_loses_authority_on_read_without_a_rewrite() {
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");

        let after_removal = CompanyContext::new("cobalt", "chief", ["chief"].map(String::from));
        let (intent, _) = read(&l, &after_removal).into_parts();
        assert!(!person_can_run(&after_removal, "quant-head", &intent));
    }

    /// THE CASE THE TEST ABOVE WAS MISNAMED FOR, asserted as the hazard it is.
    ///
    /// Departed-retention keeps a fired person in the manifest, so the read
    /// defense above does NOT fire for them: the stored fence still authorizes
    /// a departed person, and nothing in this module takes it back. That is not
    /// a defect here -- authorization is deliberately held through the offboard
    /// handoff (`org_ops::offboard_person`) -- but it means this module is the
    /// wrong place to look for the guarantee, and a reader who trusted the old
    /// test name would have believed it was covered.
    #[test]
    fn a_departed_person_is_still_authorized_by_the_stored_fence_because_the_manifest_keeps_them() {
        let mut l = ledgers();
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");

        // Departed-retention: the person is STILL a manifest member.
        let after_offboard = ctx();
        let (intent, _) = read(&l, &after_offboard).into_parts();
        assert!(
            person_can_run(&after_offboard, "quant-head", &intent),
            "the read defense is manifest membership, and a departed person is still a member -- \
             their de-authorization is the converge withdrawal's job, not this read's"
        );
    }

    #[test]
    fn the_stored_timestamp_is_the_commits_wall_reading() {
        let mut l = Ledgers::empty(WallMillis(1_752_883_200_123));
        add_from_ledger(&mut l, &ctx(), ["quant-head".to_string()]).expect("accepted");
        let stored: LaunchIntentBody =
            serde_json::from_str(l.document_body(LaunchIntentStore::NAME).expect("row"))
                .expect("parses");
        assert_eq!(stored.updated_at, "2025-07-19T00:00:00.123Z");
    }
}

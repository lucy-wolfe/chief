//! Effect-delivery tests. Real `Ledgers`, the northstar manifest, no mocks and
//! no sleeps. Effects are constructed directly through the draft's own
//! `enqueue_effect`, so the dispatch batching and the delivery breaker can
//! each be pinned without driving a whole reconcile pass.

use super::super::*;
use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::clock::WallMillis;
use crate::ledger::Ledgers;
use crate::store::organization::{self, OrganizationManifest};
use crate::test_support::northstar_manifest;

const EPOCH: i64 = 1_784_116_800_000;
const RESEARCHER: &str = "signal-researcher";

struct World {
    ledgers: Ledgers,
    manifest: OrganizationManifest,
}

impl World {
    fn new() -> Self {
        let mut ledgers = Ledgers::empty(WallMillis(EPOCH));
        let manifest = northstar_manifest(EPOCH);
        organization::create(&mut ledgers, &manifest).expect("manifest");
        seed(&mut ledgers, &manifest).expect("supervision");
        Self { ledgers, manifest }
    }

    fn enqueue(&mut self, id: &str, kind: &str, payload: BTreeMap<String, Value>) {
        let manifest = self.manifest.clone();
        let id = id.to_string();
        let kind = kind.to_string();
        mutate(&mut self.ledgers, &manifest, move |draft, at| {
            draft.enqueue_effect(&id, &kind, payload, at)?;
            Ok(())
        })
        .expect("enqueue effect");
    }

    fn enqueue_reminder(&mut self, id: &str, person: &str) {
        self.enqueue(
            id,
            "person_reminder",
            [("personId".to_string(), json!(person))].into_iter().collect(),
        );
    }

    fn enqueue_kind(&mut self, id: &str, kind: &str) {
        self.enqueue(id, kind, BTreeMap::new());
    }

    fn set_status(&mut self, id: &str, status: EffectStatus) {
        let manifest = self.manifest.clone();
        let id = id.to_string();
        mutate(&mut self.ledgers, &manifest, move |draft, _at| {
            if let Some(effect) = draft.ledger.effects.get_mut(&id) {
                effect.status = status;
            }
            draft.touch_effect(&id);
            Ok(())
        })
        .expect("set status");
    }

    fn plan(&self) -> DispatchPlan {
        dispatch_plan(&self.ledger())
    }

    fn mark(&mut self, ids: &[&str]) -> Vec<String> {
        let manifest = self.manifest.clone();
        let ids: Vec<String> = ids.iter().map(|s| (*s).to_string()).collect();
        mark_delivered(&mut self.ledgers, &manifest, &ids).expect("mark delivered")
    }

    fn record_failure(&mut self, id: &str) -> EffectStatus {
        let manifest = self.manifest.clone();
        record_delivery_failure(&mut self.ledgers, &manifest, id).expect("record failure")
    }

    fn reopen(&mut self) -> Vec<String> {
        let manifest = self.manifest.clone();
        reopen_failed_effects(&mut self.ledgers, &manifest).expect("reopen")
    }

    fn ledger(&self) -> SupervisionLedger {
        read(&self.ledgers, &self.manifest).expect("readable")
    }

    fn effect(&self, id: &str) -> Effect {
        self.ledger().effect(id).cloned().expect("effect present")
    }
}

fn has(ids: &[String], id: &str) -> bool {
    ids.iter().any(|value| value == id)
}

// --- dispatch batching -------------------------------------------------------

#[test]
fn urgent_kinds_form_their_own_batch() {
    let mut world = World::new();
    world.enqueue_kind("watch-1", "manager_goal_watch");
    world.enqueue_kind("stall-1", "manager_goal_stalled");

    let plan = world.plan();

    assert!(has(&plan.routine, "watch-1"));
    assert!(has(&plan.urgent, "stall-1"));
    assert!(!has(&plan.routine, "stall-1"), "an escalation is never routine");
}

#[test]
fn a_delivered_effect_is_no_longer_dispatchable() {
    let mut world = World::new();
    world.enqueue_reminder("del-1", RESEARCHER);
    assert!(has(&world.plan().routine, "del-1"));

    world.mark(&["del-1"]);
    assert_eq!(world.effect("del-1").status, EffectStatus::Delivered);
    assert!(!has(&world.plan().routine, "del-1"), "a delivered effect is not re-dispatched");
}

// --- the breaker: trips at three, never half-opens, operator-only reopen -----

#[test]
fn the_delivery_breaker_trips_at_three_and_never_half_opens() {
    let mut world = World::new();
    world.enqueue_reminder("del-1", RESEARCHER);

    assert_eq!(world.record_failure("del-1"), EffectStatus::Pending, "1st failure");
    assert_eq!(world.record_failure("del-1"), EffectStatus::Pending, "2nd failure");
    assert_eq!(world.record_failure("del-1"), EffectStatus::Failed, "3rd trips the breaker");
    assert_eq!(world.effect("del-1").status, EffectStatus::Failed);

    // A failed effect stays failed and is never re-driven on its own: recording
    // again is a no-op, the count does not climb, and it is not dispatchable.
    assert_eq!(world.record_failure("del-1"), EffectStatus::Failed, "still failed");
    assert_eq!(world.effect("del-1").delivery_failure_count, Some(3), "count is not bumped past 3");
    let plan = world.plan();
    assert!(!has(&plan.routine, "del-1") && !has(&plan.urgent, "del-1"), "no half-open");

    // Only an explicit operator reopen re-drives it — bounded, and it resets the
    // failure state.
    assert_eq!(world.reopen(), vec!["del-1".to_string()]);
    let effect = world.effect("del-1");
    assert_eq!(effect.status, EffectStatus::Pending);
    assert_eq!(effect.reopen_count, Some(1));
    assert_eq!(effect.delivery_failure_count, Some(0));
    assert!(effect.failed_at.is_none());
    assert!(has(&world.plan().routine, "del-1"), "reopened, dispatchable again");
}

#[test]
fn reopen_is_bounded_and_touches_only_failed_effects() {
    let mut world = World::new();
    world.enqueue_reminder("del-1", RESEARCHER); // stays pending
    assert!(world.reopen().is_empty(), "a pending effect is not reopened");
    world.set_status("del-1", EffectStatus::Failed);
    // Exhaust the reopen budget (limit 3): reopen, re-fail, three times.
    for _ in 0..3 {
        assert_eq!(world.reopen(), vec!["del-1".to_string()]);
        world.set_status("del-1", EffectStatus::Failed);
    }
    assert_eq!(world.effect("del-1").reopen_count, Some(3));
    assert!(world.reopen().is_empty(), "the reopen budget is exhausted");
}

//! Post-commit acknowledgement for Rust-owned bench convergence.
//!
//! The durable lifecycle transaction is the operation authority. This module
//! only bridges that committed operation to a later observation made by the
//! daemon's existing tagged runtime gatherer; it never polls or actuates the runtime.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use chiefd_core::store::org_ops::BenchCompletionKey;
use chiefd_core::store::supervision::{CycleInput, IdentityObservation};
use tokio::sync::oneshot;

/// In-memory waiters for committed bench operations.
///
/// A daemon restart deliberately drops this registry. Durable desired-off
/// state still converges on startup, but an interrupted HTTP request can never
/// receive a false success acknowledgement from a different process.
#[derive(Default)]
pub struct BenchCompletionRegistry {
    waiters: Mutex<HashMap<BenchCompletionKey, oneshot::Sender<()>>>,
}

impl BenchCompletionRegistry {
    fn waiters(&self) -> MutexGuard<'_, HashMap<BenchCompletionKey, oneshot::Sender<()>>> {
        self.waiters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register the exact committed operation after its writer transaction has
    /// released. The returned receiver is completed only by [`Self::observe`].
    pub fn register(&self, key: BenchCompletionKey) -> oneshot::Receiver<()> {
        let (sender, receiver) = oneshot::channel();
        self.waiters().insert(key, sender);
        receiver
    }

    /// Remove a timed-out request without changing durable convergence.
    pub fn cancel(&self, key: &BenchCompletionKey) {
        self.waiters().remove(key);
    }

    /// Whether a post-actuation observation is currently needed.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.waiters().is_empty()
    }

    /// Resolve only operations whose person is absent from a fresh, available,
    /// owned tagged audit. Suppressed and foreign inputs carry deliberately
    /// empty audits, so accepting either would turn "not observed" into a false
    /// topology proof.
    pub fn observe(&self, input: &CycleInput) {
        if input.suppressed
            || input.audit.unavailable
            || !matches!(input.identity, IdentityObservation::Owned)
        {
            return;
        }

        let mut waiters = self.waiters();
        let completed: Vec<_> = waiters
            .keys()
            .filter(|key| !input.audit.live.contains(&key.person_id))
            .cloned()
            .collect();
        for key in completed {
            if let Some(sender) = waiters.remove(&key) {
                let _ = sender.send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use chiefd_core::store::supervision::RuntimeAuditObservation;

    fn key(operation: &str, person: &str) -> BenchCompletionKey {
        BenchCompletionKey { operation_id: operation.to_string(), person_id: person.to_string() }
    }

    fn owned_live(people: &[&str]) -> CycleInput {
        CycleInput {
            audit: RuntimeAuditObservation {
                live: people.iter().map(|person| (*person).to_string()).collect(),
                ..RuntimeAuditObservation::default()
            },
            ..CycleInput::default()
        }
    }

    #[test]
    fn exact_operation_generation_keys_do_not_overwrite_each_other() {
        let registry = BenchCompletionRegistry::default();
        let first = key("transition:1:quinn:park", "quinn");
        let second = key("transition:2:quinn:park", "quinn");
        let mut first_wait = registry.register(first.clone());
        let mut second_wait = registry.register(second.clone());

        registry.cancel(&first);

        assert!(
            matches!(first_wait.try_recv(), Err(oneshot::error::TryRecvError::Closed)),
            "cancelling the exact key closes only its receiver"
        );
        assert!(
            matches!(second_wait.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "a different operation/generation remains registered"
        );
        assert!(registry.has_pending());
    }

    #[test]
    fn a_live_tagged_person_never_resolves_until_a_later_absence() {
        let registry = BenchCompletionRegistry::default();
        let mut wait = registry.register(key("transition:1:quinn:park", "quinn"));

        registry.observe(&owned_live(&["chief", "quinn"]));
        assert!(
            matches!(wait.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "the requested person's live tagged pane blocks completion"
        );

        registry.observe(&owned_live(&["chief"]));
        assert_eq!(
            wait.try_recv(),
            Ok(()),
            "a fresh tagged absence resolves the committed operation"
        );
        assert!(!registry.has_pending());
    }

    #[test]
    fn empty_suppressed_foreign_and_unavailable_audits_are_not_proof() {
        let registry = BenchCompletionRegistry::default();
        let mut wait = registry.register(key("transition:1:quinn:park", "quinn"));

        registry.observe(&CycleInput { suppressed: true, ..owned_live(&[]) });
        registry.observe(&CycleInput {
            identity: IdentityObservation::Foreign { holder: "other".to_string() },
            ..owned_live(&[])
        });
        registry.observe(&CycleInput {
            audit: RuntimeAuditObservation {
                live: BTreeSet::new(),
                unavailable: true,
                ..RuntimeAuditObservation::default()
            },
            ..CycleInput::default()
        });

        assert!(
            matches!(wait.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "an audit that did not inspect owned runtime cannot acknowledge"
        );
        assert!(registry.has_pending());
    }
}

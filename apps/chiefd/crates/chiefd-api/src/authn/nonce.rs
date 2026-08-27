//! The challenge-nonce store (agent-auth P0, R2).
//!
//! In-memory, single-use, TTL-bounded, and CAPPED per identity. A nonce is
//! bound to the `identityId` it was issued for, so a valid agent cannot answer
//! another identity's challenge. Consuming a nonce removes it (single-use), and
//! the per-identity cap with oldest-eviction keeps an abusive or buggy caller
//! from growing the map without bound — no timer sweeps it (reactive, never
//! polling); expired entries are pruned lazily on the next issue/consume for
//! that identity.
//!
//! Time is passed in (`now_ms`) rather than read here, so the store is
//! deterministic under test and never reaches for a forbidden clock.

use std::collections::{HashMap, VecDeque};

/// A nonce challenge in flight.
struct Entry {
    nonce: String,
    identity_id: String,
    expires_at_ms: i64,
}

/// What [`NonceStore::issue`] hands back to put in the challenge response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// Opaque handle the agent returns with its signature.
    pub nonce_id: String,
    /// The nonce the agent signs (inside the domain-separated message).
    pub nonce: String,
}

/// The bounded nonce store. Wrap in a `Mutex` for shared access.
pub struct NonceStore {
    entries: HashMap<String, Entry>,
    order_per_identity: HashMap<String, VecDeque<String>>,
    ttl_ms: i64,
    max_per_identity: usize,
}

impl NonceStore {
    /// A store with the given TTL and per-identity cap. A cap of 0 is treated as
    /// 1 (at least one challenge may be outstanding).
    #[must_use]
    pub fn new(ttl_ms: i64, max_per_identity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order_per_identity: HashMap::new(),
            ttl_ms,
            max_per_identity: max_per_identity.max(1),
        }
    }

    /// Record an issued challenge, evicting the identity's oldest outstanding
    /// nonces first (expired ones, then — if still at cap — the least recent).
    pub fn issue(&mut self, identity_id: &str, nonce_id: &str, nonce: &str, now_ms: i64) {
        self.prune_identity(identity_id, now_ms);
        let queue = self.order_per_identity.entry(identity_id.to_string()).or_default();
        while queue.len() >= self.max_per_identity {
            if let Some(oldest) = queue.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        queue.push_back(nonce_id.to_string());
        self.entries.insert(
            nonce_id.to_string(),
            Entry {
                nonce: nonce.to_string(),
                identity_id: identity_id.to_string(),
                expires_at_ms: now_ms.saturating_add(self.ttl_ms),
            },
        );
    }

    /// Consume a nonce by handle. Returns `Some((identity_id, nonce))` exactly
    /// once for a live, unexpired nonce; every later or expired attempt is
    /// `None`. Single-use: the entry is removed whether or not it had expired.
    pub fn consume(&mut self, nonce_id: &str, now_ms: i64) -> Option<(String, String)> {
        let entry = self.entries.remove(nonce_id)?;
        if let Some(queue) = self.order_per_identity.get_mut(&entry.identity_id) {
            queue.retain(|id| id != nonce_id);
        }
        if now_ms > entry.expires_at_ms {
            return None;
        }
        Some((entry.identity_id, entry.nonce))
    }

    /// Drop this identity's expired entries. Called on the identity's own path,
    /// so an idle identity costs nothing.
    fn prune_identity(&mut self, identity_id: &str, now_ms: i64) {
        let Some(queue) = self.order_per_identity.get_mut(identity_id) else {
            return;
        };
        queue.retain(|nonce_id| match self.entries.get(nonce_id) {
            Some(entry) if now_ms > entry.expires_at_ms => {
                self.entries.remove(nonce_id);
                false
            }
            Some(_) => true,
            None => false,
        });
        if queue.is_empty() {
            self.order_per_identity.remove(identity_id);
        }
    }

    /// Outstanding nonce count (tests / diagnostics).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no outstanding nonces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_then_consume_returns_the_bound_identity_and_nonce() {
        let mut store = NonceStore::new(1000, 4);
        store.issue("person:a", "nid-1", "nonce-1", 0);
        assert_eq!(store.consume("nid-1", 500), Some(("person:a".into(), "nonce-1".into())));
    }

    #[test]
    fn a_nonce_is_single_use() {
        let mut store = NonceStore::new(1000, 4);
        store.issue("person:a", "nid-1", "nonce-1", 0);
        assert!(store.consume("nid-1", 10).is_some());
        assert_eq!(store.consume("nid-1", 10), None, "replay of a consumed nonce is rejected");
        assert!(store.is_empty());
    }

    #[test]
    fn an_expired_nonce_is_rejected_and_removed() {
        let mut store = NonceStore::new(100, 4);
        store.issue("person:a", "nid-1", "nonce-1", 0);
        assert_eq!(store.consume("nid-1", 101), None, "past TTL is rejected");
        assert!(store.is_empty(), "expired entry is dropped on consume");
    }

    #[test]
    fn an_unknown_nonce_id_is_none() {
        let mut store = NonceStore::new(100, 4);
        assert_eq!(store.consume("never-issued", 0), None);
    }

    #[test]
    fn the_per_identity_cap_evicts_the_oldest() {
        let mut store = NonceStore::new(10_000, 2);
        store.issue("person:a", "nid-1", "n1", 0);
        store.issue("person:a", "nid-2", "n2", 1);
        store.issue("person:a", "nid-3", "n3", 2); // evicts nid-1
        assert_eq!(store.consume("nid-1", 3), None, "oldest was evicted");
        assert!(store.consume("nid-2", 3).is_some());
        assert!(store.consume("nid-3", 3).is_some());
    }

    #[test]
    fn the_cap_is_per_identity_not_global() {
        let mut store = NonceStore::new(10_000, 1);
        store.issue("person:a", "a1", "n", 0);
        store.issue("person:b", "b1", "n", 0);
        // Different identities each keep their single slot.
        assert!(store.consume("a1", 1).is_some());
        assert!(store.consume("b1", 1).is_some());
    }

    #[test]
    fn expired_entries_are_pruned_lazily_on_next_issue() {
        let mut store = NonceStore::new(100, 8);
        store.issue("person:a", "old", "n", 0);
        // A later issue for the same identity prunes the expired 'old'.
        store.issue("person:a", "new", "n", 200);
        assert_eq!(store.len(), 1);
        assert_eq!(store.consume("old", 200), None);
    }
}

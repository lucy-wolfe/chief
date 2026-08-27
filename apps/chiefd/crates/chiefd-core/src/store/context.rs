//! The manifest facts a store decode has to check against.
//!
//! Both M7 stores validate their body against the company they were read for —
//! the port of the TS `validate()` guards in `org-launch-intent.ts:42-53` and
//! `org-fleet-suppression.ts:26-37`. That check is not decoration: a
//! launch-intent ledger belonging to a different company or a stale runtime
//! session is exactly as untrustworthy as unparseable bytes, and must take the
//! same polarity path. Keeping the facts in one struct means a new store cannot
//! "forget" to check one of them by taking fewer parameters.

use std::collections::BTreeSet;

/// The manifest facts a store body is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanyContext {
    slug: String,
    chief_person_id: String,
    people_order: Vec<String>,
}

impl CompanyContext {
    /// Build a context from the current manifest.
    ///
    /// `people_order` is the manifest's canonical person ordering; it doubles
    /// as the set of known people, because "is this person in the manifest"
    /// and "where does this person sort" are answered from the same list in
    /// the TS implementation and drifting them apart would be a silent bug.
    #[must_use]
    pub fn new(
        slug: impl Into<String>,
        chief_person_id: impl Into<String>,
        people_order: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            slug: slug.into(),
            chief_person_id: chief_person_id.into(),
            people_order: people_order.into_iter().collect(),
        }
    }

    /// The company slug.
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// The root department head.
    ///
    /// Never fenced OUT: `person_can_run` and `LaunchFence::admits` both
    /// short-circuit on it, so no fence can refuse the root. It IS stored in
    /// launch intent when something asks for it — that entry is the root's
    /// start decision, and since #1148 deleted the unconditional
    /// `OrganizationRoot` lease it is the only thing that makes the root run.
    /// This doc used to read "never stored in launch intent (it is implicitly
    /// intended)", which is the assumption the lease's deletion retired.
    #[must_use]
    pub fn chief_person_id(&self) -> &str {
        &self.chief_person_id
    }

    /// The manifest's person ordering.
    #[must_use]
    pub fn people_order(&self) -> &[String] {
        &self.people_order
    }

    /// Whether `person_id` is in the current manifest.
    #[must_use]
    pub fn knows_person(&self, person_id: &str) -> bool {
        self.people_order.iter().any(|known| known == person_id)
    }

    /// `person_ids` sorted into manifest order, unknown ids dropped, duplicates
    /// collapsed. The canonical shape of a stored fence.
    ///
    /// TOMBSTONE: `&& *person_id != &self.chief_person_id` — this used to say
    /// "CEO removed", because the root was never stored in a fence.
    ///
    /// That was correct while `activity::reconcile` gave the CEO an
    /// unconditional `OrganizationRoot` lease: the root ran unasked, so a fence
    /// naming it carried no information and dropping it kept one canonical
    /// shape. #1148 deleted the lease, `prepare_ceo_only` now writes the root's
    /// start decision AS a launch-intent entry, and this filter then silently
    /// ATE that decision on the next write — every `add` (a mail grant) and
    /// every `remove` (a settle withdrawal) re-canonicalizes the whole fence
    /// through here, so the first time anybody else was granted or withdrawn
    /// the root lost its only demand and the company drained to empty a quiet
    /// lease later.
    ///
    /// The root is still never FENCED OUT: `person_can_run` and
    /// `LaunchFence::admits` both short-circuit on the CEO, so the fence stays
    /// permissive toward it. What it may now do is NAME it.
    ///
    /// POSTSCRIPT, and read it before concluding the filter can come back: the
    /// operator reversed the settle ruling for the root alone on 2026-08-14
    /// ("the CEO can never go to sleep"), so the lease is restored and a fence
    /// entry is no longer the root's ONLY demand. The filter stays deleted
    /// anyway, and the reason is now the simpler one: `prepare_ceo_only` writes
    /// that row, and a canonicalizer that silently drops a row it was handed is
    /// wrong whether or not something else happens to keep the root alive. The
    /// eviction was invisible precisely because a lease was masking it.
    pub(crate) fn in_manifest_order(&self, person_ids: &BTreeSet<String>) -> Vec<String> {
        self.people_order
            .iter()
            .filter(|person_id| person_ids.contains(*person_id))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> CompanyContext {
        CompanyContext::new(
            "cobalt",
            "chief",
            ["chief", "quant-head", "signal-researcher"].map(String::from),
        )
    }

    /// WHAT THIS USED TO CLAIM: `manifest_order_wins_over_caller_order_and_
    /// drops_the_ceo` asserted that the root is filtered OUT of a canonicalized
    /// fence — the stored fence named non-CEO people only, because the root ran
    /// on an unconditional `OrganizationRoot` lease and could not be asked for.
    ///
    /// #1148 deleted that lease and `prepare_ceo_only` now writes the root's
    /// start decision as a fence entry, so dropping it here erased the only
    /// demand the root has. The ordering half of the claim is unchanged and is
    /// still asserted; only the eviction is retired.
    #[test]
    fn manifest_order_wins_over_caller_order_and_keeps_the_ceo() {
        let ctx = context();
        let requested: BTreeSet<String> =
            ["signal-researcher", "chief", "quant-head"].map(String::from).into_iter().collect();
        assert_eq!(
            ctx.in_manifest_order(&requested),
            vec!["chief".to_string(), "quant-head".to_string(), "signal-researcher".to_string()],
            "manifest order, and the root is a nameable member of a fence like anybody else"
        );
    }

    #[test]
    fn unknown_people_are_not_manifest_ordered_into_existence() {
        let ctx = context();
        let requested: BTreeSet<String> = ["ghost"].map(String::from).into_iter().collect();
        assert!(ctx.in_manifest_order(&requested).is_empty());
        assert!(!ctx.knows_person("ghost"));
        assert!(ctx.knows_person("quant-head"));
    }
}

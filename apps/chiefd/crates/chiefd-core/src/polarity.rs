//! Failure polarity, encoded in types — **three** markers, not two.
//!
//! Plan §5.5. What a store does when its bytes are unreadable is a
//! per-(store × operation) decision with a long history of being got wrong in
//! exactly one direction, so it is a type-level property here rather than a
//! convention:
//!
//! | Marker | On corrupt bytes | Examples |
//! |---|---|---|
//! | [`FailOpen`] | `empty()` + a warning | health, memory-job, `startupAdmissionUntil` |
//! | [`FailSafeValue`] | the **restrictive** value | launch intent → deny-all |
//! | [`FailClosed`] | `Corrupt{store}` | removal journals, registry, learned skills, loop control |
//!
//! Polarity is still a per-*operation* decision, not a per-store one: the macro
//! takes a marker for each of read/write/clear (`tests/polarity_matrix.rs`,
//! TESTING.md §3.2). The store that once used opposite polarities on opposite
//! operations — fleet-suppression, `FailSafeValue` read and `FailClosed`
//! write/clear — was deleted with the CEO-only-is-a-boot decision, so no
//! surviving store exercises the split; the axis remains because the decision
//! it forces is worth keeping available.
//!
//! # Why the registry is a macro (M7)
//!
//! The failure this module exists to prevent is not "someone chose the wrong
//! polarity" — a reviewer catches that. It is **"someone added a store and
//! never made the decision at all"**, which is how the predecessor system
//! ended up with a launch-intent fence whose absence meant *no fence* rather
//! than *fence with nobody allowed*. So:
//!
//! - [`StoreKind`] is **sealed**: outside this crate a store cannot be defined
//!   at all, and inside it the only thing that implements the seal is
//!   [`declare_stores!`].
//! - [`declare_stores!`] takes a polarity for **every** operation in
//!   [`StoreOp::ALL`]. Omitting one is a macro match failure, i.e. a compile
//!   error, not a default.
//! - The macro emits a compile-time assertion that the store actually
//!   implements the marker trait it was declared with, so the declaration and
//!   the behaviour cannot drift.
//! - Declaring the same store twice is a duplicate enum variant: a compile
//!   error.
//!
//! What a macro cannot check — that the *set* of declared stores still equals
//! the plan's inventory — is checked by
//! [`STORE_POLARITY_INVENTORY`](crate::store::STORE_POLARITY_INVENTORY) and the
//! matrix test.

use crate::error::ChiefdError;

/// Sealing for [`StoreKind`].
///
/// `pub(crate)` on purpose: reachable from [`declare_stores!`] expansions
/// inside this crate, unnameable from any other crate. The guard test
/// `only_the_registry_macro_implements_the_store_seal` pins that the sole
/// in-crate implementor is the macro.
pub(crate) mod sealed {
    /// Implemented only by [`crate::declare_stores!`].
    pub trait Sealed {}
}

/// A durable store chiefd owns.
///
/// Sealed: a store exists only by way of [`declare_stores!`], which is the
/// only place a polarity decision can be recorded.
pub trait StoreKind: sealed::Sealed {
    /// Wire/diagnostic name; appears in `Corrupt{store}` and in warnings. It
    /// doubles as the `documents.store` key, which is why the containment test
    /// requires the literal to appear in exactly one production source file.
    const NAME: &'static str;
    /// The decoded body type.
    type Body;
}

/// Corruption is survivable: read as empty and warn.
///
/// Correct only where a missing value cannot grant authority. Health data is
/// the archetype — losing it degrades observability, not safety.
pub trait FailOpen: StoreKind {
    /// The value a corrupt or absent store reads as.
    fn empty() -> Self::Body;
}

/// Corruption resolves to a **deny** value, not to absence.
///
/// The distinction the two-trait model could not express: launch intent must
/// read as `Fenced(∅)` — deny everyone — when unreadable. The
/// [`LaunchIntent`](crate::store::launch_intent::LaunchIntent) type has no
/// permissive variant at all, so "absence is permissive" is not merely wrong
/// but unrepresentable.
pub trait FailSafeValue: StoreKind {
    /// The most restrictive value this store can hold.
    fn restrictive() -> Self::Body;
}

/// Corruption is an error the caller must see.
///
/// Invariant 40: a corrupt removal journal blocks, it never restarts; a
/// suppression `clear()` refuses rather than guessing.
pub trait FailClosed: StoreKind {}

/// The three polarities. There is no fourth, and there is no default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Polarity {
    /// [`FailOpen`]: corrupt bytes read as `empty()` plus a warning.
    FailOpen,
    /// [`FailSafeValue`]: corrupt bytes read as `restrictive()` plus a warning.
    FailSafeValue,
    /// [`FailClosed`]: corrupt bytes are `Corrupt{store}`.
    FailClosed,
}

impl Polarity {
    /// Stable name used in the matrix test's failure messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FailOpen => "FailOpen",
            Self::FailSafeValue => "FailSafeValue",
            Self::FailClosed => "FailClosed",
        }
    }
}

/// The operation axis of the polarity matrix.
///
/// Polarity is per (store × operation) because fleet suppression genuinely
/// needs opposite answers on its read and write paths: an unreadable marker
/// must *report* SUPPRESSED (a value the operator can act on) while
/// `clear()` must *refuse* — clearing is what wakes 28 agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StoreOp {
    /// Decode the store for a caller.
    Read,
    /// Replace the store body.
    Write,
    /// Remove the store body entirely.
    Clear,
}

impl StoreOp {
    /// Every operation. The matrix is the full cross product with the stores.
    pub const ALL: &'static [StoreOp] = &[StoreOp::Read, StoreOp::Write, StoreOp::Clear];

    /// Stable name used in the matrix test's failure messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Clear => "clear",
        }
    }
}

/// One cell of the polarity matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolarityRow {
    /// The store's wire name.
    pub store: &'static str,
    /// The operation.
    pub op: StoreOp,
    /// The polarity that cell must have.
    pub polarity: Polarity,
}

/// The outcome of decoding a store's bytes, tagged with the polarity that
/// produced it. The polarity matrix test asserts against this.
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded<B> {
    /// Bytes parsed.
    Value(B),
    /// Bytes did not parse and the store is [`FailOpen`]: empty + warning.
    RecoveredEmpty {
        /// The empty value.
        body: B,
        /// Warning surfaced to the caller in `warnings[]`.
        warning: String,
    },
    /// Bytes did not parse and the store is [`FailSafeValue`]: deny value.
    RecoveredRestrictive {
        /// The restrictive value.
        body: B,
        /// Warning surfaced to the caller in `warnings[]`.
        warning: String,
    },
    /// **No row was ever written.** Not a recovery: nothing failed, so nothing
    /// is reported as having failed.
    ///
    /// This variant exists because one `None` used to decide two unrelated
    /// things — what value absence produces, and what is said about it — so a
    /// read that folded absence into a recovering arm told an operator that a
    /// document which had never been written `was unreadable`. That is a
    /// sentence asserting a cause that did not happen.
    ///
    /// The two decisions are separate here and both belong to the CALLER:
    ///
    /// - `body` is the store's absence value, named at the read site. It is
    ///   **not** derived from the polarity marker. A [`FailSafeValue`] store
    ///   whose fence has never been written still denies: "absence is not
    ///   corruption" must never become "absence is permissive".
    /// - `note` is what to say about it, and is `None` at every store where an
    ///   absent row is the ordinary first-pass state. A note is not a warning;
    ///   [`recovered`](Self::recovered) stays `false` either way.
    Absent {
        /// The value this store reads as when no row was ever written.
        body: B,
        /// An honest sentence about the absence, or `None` to say nothing.
        note: Option<String>,
    },
}

impl<B> Decoded<B> {
    /// Split into the body every caller gets and the optional warning.
    ///
    /// Deliberately infallible: a `Decoded` only exists for the two polarities
    /// that always produce a body. [`FailClosed`] never reaches this type.
    pub fn into_parts(self) -> (B, Option<String>) {
        match self {
            Self::Value(body) => (body, None),
            Self::RecoveredEmpty { body, warning }
            | Self::RecoveredRestrictive { body, warning } => (body, Some(warning)),
            // A note travels the channel a warning travels — `warnings[]` is
            // the only one a read has — but it is optional, because most
            // absent rows are worth no words at all.
            Self::Absent { body, note } => (body, note),
        }
    }

    /// Whether this decode recovered from unreadable bytes.
    ///
    /// `false` for [`Absent`](Self::Absent): a row that was never written did
    /// not fail to be read, so there was nothing to recover from.
    #[must_use]
    pub const fn recovered(&self) -> bool {
        matches!(self, Self::RecoveredEmpty { .. } | Self::RecoveredRestrictive { .. })
    }

    /// The value for a store whose row was never written, with nothing said.
    ///
    /// The quiet case, which is most of them: a fresh company has written none
    /// of its rows yet, and that is its ordinary state, not an event.
    #[must_use]
    pub const fn absent(body: B) -> Self {
        Self::Absent { body, note: None }
    }
}

/// Why a store's bytes did not become a value.
///
/// A sentence, not a type, because its only destination is the `warning` a
/// [`Decoded`] carries to `warnings[]` — and because the reasons are not one
/// taxonomy: a `serde` failure, a schema version that is not this one, and a
/// perfectly readable ledger belonging to ANOTHER company are three different
/// facts about three different things, and each store knows its own.
pub type DecodeRefusal = String;

/// Decode helper for a [`FailOpen`] store — for a row that **exists**.
///
/// It judges bytes, so it is only ever handed bytes: absence never reaches it,
/// and is answered by the read site with [`Decoded::Absent`]. That is why the
/// `parse` functions feeding these helpers take `&str` rather than
/// `Option<&str>` — the type makes the collapse of "never written" into
/// "unreadable" impossible rather than merely discouraged.
///
/// Takes the parse OUTCOME, never an `Option`. The judge cannot name a cause
/// it was never given: every caller used to `.ok()` the reason away one line
/// above this call, so a store that decoded fine but belonged to another
/// company, one whose schema version had moved, and one whose bytes were
/// truncated all produced the same four words — and the recovery those four
/// words announce (a reset) is exactly when an operator needs to know which
/// of the three happened.
pub fn decode_fail_open<S: FailOpen>(parsed: Result<S::Body, DecodeRefusal>) -> Decoded<S::Body> {
    match parsed {
        Ok(body) => Decoded::Value(body),
        Err(cause) => Decoded::RecoveredEmpty {
            body: S::empty(),
            warning: format!("store {} was unreadable ({cause}); continuing empty", S::NAME),
        },
    }
}

/// Decode helper for a [`FailSafeValue`] store — for a row that **exists**.
///
/// Absence is the read site's decision, not this helper's, and here that split
/// is a safety property rather than a wording one: the value an absent fence
/// produces stays restrictive, and only the sentence changes.
///
/// Takes the parse outcome for the same reason [`decode_fail_open`] does, and
/// it matters more here: this arm DENIES, and "why is everyone fenced out"
/// is unanswerable from a warning that only says the bytes were unreadable.
pub fn decode_fail_safe_value<S: FailSafeValue>(
    parsed: Result<S::Body, DecodeRefusal>,
) -> Decoded<S::Body> {
    match parsed {
        Ok(body) => Decoded::Value(body),
        Err(cause) => Decoded::RecoveredRestrictive {
            body: S::restrictive(),
            warning: format!(
                "store {} was unreadable ({cause}); applying the restrictive value",
                S::NAME
            ),
        },
    }
}

/// Decode helper for a [`FailClosed`] store.
///
/// The cause NO LONGER stops here. This doc said the opposite until the two
/// halves of that defect landed together: the parse outcome now arrives as a
/// `Result` rather than an `Option`, so the cause still exists by the time the
/// judge sees it, and `ChiefdError::Corrupt` now carries a `cause` field, so
/// there is somewhere to put it. It reaches the wire with the store name.
///
/// # Errors
/// Returns [`ChiefdError::Corrupt`] when the bytes did not parse.
pub fn decode_fail_closed<S: FailClosed>(
    parsed: Result<S::Body, DecodeRefusal>,
) -> Result<S::Body, ChiefdError> {
    // The synthesis of two packets that could not write this line alone.
    //
    // This helper took an `Option`, so every caller had already `.ok()`d its
    // error away BEFORE the judge could see it - the cause was destroyed
    // upstream of the code expected to report it, and no wording here could
    // have recovered it. `fix/polarity-decode-cause` changed the signature so
    // the cause ARRIVES. It then had to discard it anyway, because
    // `ChiefdError::Corrupt` carried a store name and nothing else.
    //
    // `refactor/store-failure-kind`'s remainder, landed in batch 9, gave the
    // kind a `cause` field. So the cause now arrives AND is carried: the two
    // ends of the same defect, closed together.
    parsed.map_err(|cause| crate::error::corrupt_store_because(S::NAME, cause))
}

/// Assert at compile time that `$ty` implements the marker `$polarity` names.
///
/// Not public API — [`declare_stores!`] uses it to make "declared `FailClosed`,
/// implemented `FailOpen`" a compile error rather than a review question.
#[doc(hidden)]
#[macro_export]
macro_rules! __assert_polarity_marker {
    ($ty:ty, FailOpen) => {
        const _: fn() = || {
            fn assert_marker<T: $crate::polarity::FailOpen>() {}
            assert_marker::<$ty>();
        };
    };
    ($ty:ty, FailSafeValue) => {
        const _: fn() = || {
            fn assert_marker<T: $crate::polarity::FailSafeValue>() {}
            assert_marker::<$ty>();
        };
    };
    ($ty:ty, FailClosed) => {
        const _: fn() = || {
            fn assert_marker<T: $crate::polarity::FailClosed>() {}
            assert_marker::<$ty>();
        };
    };
}

/// Declare the store registry: the single place a polarity decision is made.
///
/// ```ignore
/// declare_stores! {
///     /// docs
///     LaunchIntent => launch_intent::LaunchIntentStore {
///         read: FailSafeValue, write: FailSafeValue, clear: FailSafeValue,
///     },
/// }
/// ```
///
/// Emits [`StoreId`](crate::store::StoreId) with a variant per store,
/// `StoreId::ALL`, `StoreId::name`, `StoreId::polarity(op)`, the
/// `POLARITY_MATRIX` cross product, the [`sealed::Sealed`] impls, and one
/// compile-time marker assertion per cell.
///
/// Three failure modes are compile errors by construction: a store with no
/// entry cannot implement [`StoreKind`] (the seal); an entry missing an
/// operation does not match the macro arm; an entry whose declared polarity
/// disagrees with the implemented marker trait fails
/// [`__assert_polarity_marker!`].
#[macro_export]
macro_rules! declare_stores {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $ty:ty {
            read: $read:ident,
            write: $write:ident,
            clear: $clear:ident $(,)?
        }
    ),+ $(,)?) => {
        /// Every store chiefd owns, as a value.
        ///
        /// Generated by [`declare_stores!`]. Adding a variant by hand is not
        /// possible; adding a store without a polarity decision is not
        /// possible.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum StoreId {
            $(
                $(#[$meta])*
                $variant,
            )+
        }

        impl StoreId {
            /// Every declared store, in declaration order.
            pub const ALL: &'static [StoreId] = &[$(StoreId::$variant),+];

            /// The store's wire name — the same string as `StoreKind::NAME`.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => <$ty as $crate::polarity::StoreKind>::NAME,)+
                }
            }

            /// The polarity this store must have for `op`.
            ///
            /// Total by construction: every (store, op) pair has an arm, so
            /// there is no "unspecified" cell to fall through to a default.
            #[must_use]
            pub const fn polarity(self, op: $crate::polarity::StoreOp) -> $crate::polarity::Polarity {
                match (self, op) {
                    $(
                        (Self::$variant, $crate::polarity::StoreOp::Read) =>
                            $crate::polarity::Polarity::$read,
                        (Self::$variant, $crate::polarity::StoreOp::Write) =>
                            $crate::polarity::Polarity::$write,
                        (Self::$variant, $crate::polarity::StoreOp::Clear) =>
                            $crate::polarity::Polarity::$clear,
                    )+
                }
            }
        }

        /// The full (store × operation) polarity cross product.
        ///
        /// This is the artifact `tests/polarity_matrix.rs` drives corrupt bytes
        /// through: every cell is exercised, not merely compared.
        pub const POLARITY_MATRIX: &[$crate::polarity::PolarityRow] = &[
            $(
                $crate::polarity::PolarityRow {
                    store: <$ty as $crate::polarity::StoreKind>::NAME,
                    op: $crate::polarity::StoreOp::Read,
                    polarity: $crate::polarity::Polarity::$read,
                },
                $crate::polarity::PolarityRow {
                    store: <$ty as $crate::polarity::StoreKind>::NAME,
                    op: $crate::polarity::StoreOp::Write,
                    polarity: $crate::polarity::Polarity::$write,
                },
                $crate::polarity::PolarityRow {
                    store: <$ty as $crate::polarity::StoreKind>::NAME,
                    op: $crate::polarity::StoreOp::Clear,
                    polarity: $crate::polarity::Polarity::$clear,
                },
            )+
        ];

        $(
            impl $crate::polarity::sealed::Sealed for $ty {}
            $crate::__assert_polarity_marker!($ty, $read);
            $crate::__assert_polarity_marker!($ty, $write);
            $crate::__assert_polarity_marker!($ty, $clear);
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::launch_intent::{LaunchIntent, LaunchIntentStore};

    #[test]
    fn fail_safe_value_decode_of_unreadable_launch_intent_denies_everyone() {
        let decoded =
            decode_fail_safe_value::<LaunchIntentStore>(Err("the body did not decode".to_string()));
        assert!(decoded.recovered());
        let (body, warning) = decoded.into_parts();
        assert_eq!(body, LaunchIntentStore::restrictive());
        let warning = warning.expect("a recovered read must warn");
        assert!(warning.contains("launch-intent"), "warning names the store: {warning}");
    }

    /// VALUE AND SENTENCE, ASSERTED SEPARATELY — the whole point of the
    /// absent arm, and the one place a wording fix could become a security
    /// defect if the two were checked together.
    ///
    /// A fence nobody has written authorizes NOBODY, exactly as an unreadable
    /// one does. "Absence is not corruption" is a statement about the sentence;
    /// it must never be read as a licence to make absence permissive.
    #[test]
    fn an_unwritten_fence_denies_everyone_and_never_claims_corruption() {
        let decoded = Decoded::Absent {
            body: LaunchIntentStore::restrictive(),
            note: Some("store launch-intent has no row; it authorizes nobody".to_string()),
        };
        // The SENTENCE: nothing failed, so nothing is reported as having failed.
        assert!(!decoded.recovered(), "a row that was never written did not fail to be read");
        let (body, note) = decoded.into_parts();
        // The VALUE, checked on its own and against the same constant the
        // corrupt path resolves to. If these two ever stop agreeing, an absent
        // fence has started authorizing somebody.
        assert_eq!(
            body,
            LaunchIntentStore::restrictive(),
            "an absent fence denies exactly what an unreadable one denies"
        );
        let note = note.expect("this store's absence is worth words: its value is a refusal");
        assert!(
            !note.contains("unreadable"),
            "an unwritten document must not be reported as damaged bytes: {note}"
        );
        assert!(note.contains("authorizes nobody"), "and it must say what it does: {note}");
    }

    /// The other direction, and the regression that would matter most: fixing
    /// the wording for absence must not stop CORRUPTION being called
    /// corruption. Both arms deny; only one of them names damaged bytes.
    #[test]
    fn absence_and_corruption_produce_one_value_and_two_different_sentences() {
        let absent = Decoded::<LaunchIntent>::Absent {
            body: LaunchIntentStore::restrictive(),
            note: Some("store launch-intent has no row; it authorizes nobody".to_string()),
        };
        let corrupt =
            decode_fail_safe_value::<LaunchIntentStore>(Err("the body did not decode".to_string()));
        assert!(corrupt.recovered(), "unreadable bytes ARE a recovery");
        let (absent_body, absent_note) = absent.into_parts();
        let (corrupt_body, corrupt_warning) = corrupt.into_parts();
        assert_eq!(absent_body, corrupt_body, "ONE value: both refuse everybody");
        assert_ne!(
            absent_note, corrupt_warning,
            "TWO sentences: an operator asking why everyone is fenced out is owed the real cause"
        );
        assert!(corrupt_warning.expect("a recovery warns").contains("unreadable"));
    }

    #[test]
    fn a_parsed_body_is_never_reported_as_recovered() {
        let decoded =
            decode_fail_safe_value::<LaunchIntentStore>(Ok(LaunchIntentStore::restrictive()));
        assert!(!decoded.recovered());
        assert_eq!(decoded.into_parts().1, None);
    }

    /// The seal is what makes "a store without a polarity decision" impossible
    /// rather than merely discouraged. If a second implementor appears, some
    /// store now exists outside the registry and the matrix no longer covers
    /// the system.
    #[test]
    fn only_the_registry_macro_implements_the_store_seal() {
        fn production_half(text: &str) -> &str {
            text.split("#[cfg(test)]").next().unwrap_or(text)
        }
        let sources: Vec<(&str, &str)> = vec![
            ("polarity.rs", production_half(include_str!("polarity.rs"))),
            ("store/mod.rs", production_half(include_str!("store/mod.rs"))),
            ("store/launch_intent.rs", production_half(include_str!("store/launch_intent.rs"))),
            ("ledger.rs", production_half(include_str!("ledger.rs"))),
        ];
        let implementors: Vec<&str> = sources
            .iter()
            .filter(|(_, text)| {
                text.lines().any(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//") && line.contains("Sealed for")
                })
            })
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            implementors,
            vec!["polarity.rs"],
            "the store seal may only be implemented by declare_stores!; a hand-written \
             impl means a store exists that the polarity matrix does not cover"
        );
    }
}

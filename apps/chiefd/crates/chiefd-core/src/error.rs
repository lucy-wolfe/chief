//! The closed error taxonomy, core half.
//!
//! Plan §1: every error the wire can carry is one of `Refused`, `Conflict`,
//! `Busy`, `StoreFailure`, `Corrupt`, `Unavailable`. A non-accepted result is
//! an error status,
//! never 200-with-failure, and SQL `CHECK` violations must never surface —
//! limits are enforced in `validate(&Ledger)` and returned as `Refused` with
//! `legalRoutes`.
//!
//! **Crate split.** This module owns the *semantic* taxonomy the store layer
//! produces. Its serde/`schemars` wire projection is `chiefd-api`'s to freeze
//! at M2 (plan §9); nothing here derives `Serialize`, so the two cannot drift
//! by accident — the API crate must write an explicit conversion.

use crate::actor::BusyProof;

/// A refusal the store layer produced deliberately: the operation was legal to
/// ask for and chiefd declined, naming the routes that would have worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Stable machine code, e.g. `identity-echo-mismatch`, `ack-retry-exhausted`.
    /// The conformance corpus matches on `{type, code}` only (TESTING.md §2).
    pub code: &'static str,
    /// Human-facing sentence. Snapshot-tested separately with `insta` so copy
    /// edits do not break the corpus.
    pub message: String,
    /// What the caller *could* do instead. Empty is legal but discouraged: the
    /// whole point of the taxonomy is that a model reading the error can act.
    pub legal_routes: Vec<String>,
}

impl Refusal {
    /// Refusal with no alternative routes offered.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), legal_routes: Vec::new() }
    }

    /// Builder form: attach the legal routes.
    #[must_use]
    pub fn with_routes(mut self, routes: impl IntoIterator<Item = String>) -> Self {
        self.legal_routes = routes.into_iter().collect();
        self
    }
}

/// Every error chiefd can return. Adding a variant is an API-freeze change
/// (plan §9: cross-track types change only by PR with all track owners).
#[derive(Debug, thiserror::Error)]
pub enum ChiefdError {
    /// Deliberate decline; carries the code and legal routes.
    #[error("refused: {}: {}", .0.code, .0.message)]
    Refused(Refusal),

    /// A fence lost: a runtime-generation or lease token did
    /// not match. Both sides are reported so the caller can re-read and retry.
    ///
    /// `code` is not decoration. The conformance corpus matches every error on
    /// `{type, code}` (TESTING.md §2, `conformance/FORMAT.md`), and it records
    /// conflicts such as `{type:"Conflict", code:"claim-mismatch"}`. Without a
    /// code here, every conflict on the wire would be indistinguishable from
    /// every other one and no conflict fixture could be replayed against Rust.
    #[error("conflict ({code}): expected {expected}, actual {actual}")]
    Conflict {
        /// Stable machine code, e.g. `claim-mismatch`, `lease-fence-mismatch`.
        code: &'static str,
        /// What the caller presented.
        expected: String,
        /// What chiefd holds.
        actual: String,
    },

    /// chiefd waited the documented ladder or queue deadline and still could
    /// not proceed.
    ///
    /// The payload is [`BusyProof`], whose constructor is private to
    /// [`crate::actor`]. Handler code physically cannot mint a `Busy` — the
    /// fail-fast-with-no-retry shape that shipped three times in the TS system
    /// is unwritable here (plan §5.2 item 2, TESTING.md §3.1).
    #[error("busy: {0}")]
    Busy(BusyProof),

    /// A fail-closed store could not be read (plan §5.5): the query, the lock,
    /// the transaction or the disk failed. The DATA IS NOT KNOWN TO BE
    /// DAMAGED — that claim belongs to [`Self::Corrupt`] alone.
    ///
    /// This is the generic store-error kind, and it exists because the
    /// specific one used to do this job. `Corrupt { store }` was returned for
    /// every `SQLITE_BUSY` under a parallel gate, every `SQLITE_IOERR` from a
    /// full `/tmp`, and every ordinary constraint violation, so the single
    /// word that tells an operator their bytes are damaged fired almost
    /// entirely on runs where nothing was damaged. #1031 was seen twice and
    /// diagnosed zero times for exactly that reason, and three earlier
    /// "reproducible product defects" in this programme turned out to be a
    /// full disk wearing this label.
    ///
    /// **The kind carries its cause.** A store error that cannot say why is the
    /// same defect wearing a better name: today the reason exists only on the
    /// daemon's stderr, where the contract harness deletes it unread, so an
    /// operator sees a bare store name and nothing else. `cause` is the
    /// rendered underlying failure — the `rusqlite::Error` with its primary and
    /// extended result codes, or the `Refusal` naming the exact invariant that
    /// broke — and it travels onto the wire.
    #[error("store failure: {store}: {cause}")]
    StoreFailure {
        /// The store name, as reported on the wire.
        store: &'static str,
        /// Why it failed, rendered. Never empty: the constructors are the only
        /// door and every one of them takes a reason.
        cause: String,
    },

    /// A stored body DID NOT DECODE: the bytes are there and they are not the
    /// value they must be.
    ///
    /// This is the narrow, alarming kind, and it is now reserved for the
    /// decode step alone — `serde` failing on a document body, a ledger parser
    /// returning `None`, a column holding an enum discriminant or timestamp
    /// that cannot be read back. A lock, an I/O error, a failed query or an
    /// invariant violated by a body that decoded fine is a
    /// [`Self::StoreFailure`], because none of those is evidence of damage.
    ///
    /// Carries its cause for the same reason [`Self::StoreFailure`] does: the
    /// `serde` error naming the offending field and byte offset is the whole
    /// value of knowing a body did not decode.
    #[error("corrupt store: {store}: {cause}")]
    Corrupt {
        /// The store name, as reported on the wire.
        store: &'static str,
        /// Why the bytes did not decode, rendered.
        cause: String,
    },

    /// The store has NEVER BEEN WRITTEN — its document is absent, not damaged
    /// (#105).
    ///
    /// Absent, corrupt and empty are three different states, and before this
    /// the type had two: `document_body(...)` returning `None` was reported as
    /// [`Self::Corrupt`]. A fresh company — one whose organization manifest
    /// rows exist but whose native ledgers have never been seeded, which is
    /// MEASURABLY what
    /// `createOrganization` leaves behind — therefore refused every duty with
    /// `corrupt store: supervision`, sending an operator hunting for damaged
    /// bytes that were never written, while the daemon exited reporting
    /// success.
    ///
    /// This is deliberately NOT a fail-open. Each caller decides, because the
    /// right answer differs and one of them is dangerous: a health gather may
    /// legitimately skip an absent ledger, but a staffing reconcile must never
    /// read "absent" as "nobody is staffed" — that is the exact input that
    /// plans a kill for every person in the company.
    #[error("store never written: {store}")]
    Absent {
        /// The store name, as reported on the wire.
        store: &'static str,
    },

    /// The target is not currently serving: chiefd is still in a startup tier,
    /// or the company's writer actor is quiescing for removal (plan §7.2,
    /// §2.1). In-flight `mutate` futures resolve to this rather than hanging.
    #[error("unavailable: {reason}")]
    Unavailable {
        /// Short, stable reason string (`starting`, `removing`, …).
        reason: &'static str,
    },
}

/// Flatten a store error into [`ChiefdError::StoreFailure`], CARRYING the
/// underlying error inside the value. **This is the default door**: reach for
/// [`corrupt_store`] only when the bytes themselves failed to decode.
///
/// # Why this exists
///
/// The kind used to carry only a store NAME. Every construction site that
/// flattened a `rusqlite::Error` into it wrote `.map_err(|_| ...)` — dropping
/// the error object at the mapping point, so the cause was not merely unlogged,
/// it no longer existed. 143 sites did this, and all of them then reported the
/// alarming word `corrupt` for what may equally be `SQLITE_BUSY` under a
/// parallel gate, `SQLITE_IOERR` from a full `/tmp`, or an ordinary constraint
/// violation.
///
/// Recovering the error at the mapping point was necessary and not sufficient:
/// it went to stderr, the variant had nowhere to put it, and the caller —
/// the operator, the route, the contract harness — still received a bare store
/// name. The rendered cause now lives IN the value and reaches the wire, and
/// the stderr line is kept as well because it is the only record that survives
/// a caller which discards the error entirely.
///
/// `eprintln!` rather than `tracing::error!` is deliberate and matches the
/// precedent set in `store::organization_rows` after an opaque `corrupt store`
/// hid a deferred-FK failure for two days (#49): no `tracing` subscriber is
/// installed under `cargo test`, so a traced error is discarded in precisely
/// the context where the gate runs and where this bug has been observed.
///
/// The bound is `Debug`, not `Display`, and that is the whole point for a
/// SQLite failure: `rusqlite::Error`'s `Display` is the bare sentence
/// ("database is locked"), while its `Debug` carries the primary AND extended
/// result codes — `SqliteFailure(Error { code: DatabaseBusy, extended_code: 5 },
/// ...)`. Distinguishing `SQLITE_BUSY` from `SQLITE_IOERR` is exactly the
/// question every one of these labels has been unable to answer, and the code
/// is what answers it. `Debug` is also universal, so refusal types that flatten
/// through the same door are covered without a second helper.
#[must_use]
pub fn store_failure<E: std::fmt::Debug>(store: &'static str, err: E) -> ChiefdError {
    let cause = format!("{err:?}");
    eprintln!("[{store}] store error: {cause}");
    ChiefdError::StoreFailure { store, cause }
}

/// Flatten a DECODE failure into [`ChiefdError::Corrupt`], preserving the
/// underlying error on stderr exactly as [`store_failure`] does.
///
/// Use this one only where the stored bytes could not be turned into the value
/// they must be: `serde` rejecting a document body, a ledger parser returning
/// `None`, a column holding an enum discriminant or timestamp that cannot be
/// read back. A lock, an I/O error, a failed query, or an invariant violated by
/// a body that decoded fine is [`store_failure`] — it is not evidence that
/// anything is damaged, and saying so sends an operator hunting for bytes that
/// are perfectly intact.
#[must_use]
pub fn corrupt_store<E: std::fmt::Debug>(store: &'static str, err: E) -> ChiefdError {
    let cause = format!("{err:?}");
    eprintln!("[{store}] store decode failure: {cause}");
    ChiefdError::Corrupt { store, cause }
}

/// [`store_failure`] for a site that has a REASON rather than an error object.
///
/// Some failures have no `Err` to render: a parser that answered `None`, a
/// column holding a discriminant outside its vocabulary, a reconstruct that
/// came back empty immediately after its own publish. Those sites know exactly
/// what went wrong and used to write it in a comment, where no caller could
/// read it. They write it here instead.
#[must_use]
pub fn store_failure_because(store: &'static str, cause: impl Into<String>) -> ChiefdError {
    let cause = cause.into();
    eprintln!("[{store}] store error: {cause}");
    ChiefdError::StoreFailure { store, cause }
}

/// [`corrupt_store`] for a decode site that has a REASON rather than an error
/// object — a `parse` that answered `None`, a stored discriminant outside its
/// vocabulary, an unmodeled key the read allowlist rejects.
#[must_use]
pub fn corrupt_store_because(store: &'static str, cause: impl Into<String>) -> ChiefdError {
    let cause = cause.into();
    eprintln!("[{store}] store decode failure: {cause}");
    ChiefdError::Corrupt { store, cause }
}

impl ChiefdError {
    /// Shorthand for the common `Refused` construction.
    pub fn refused(code: &'static str, message: impl Into<String>) -> Self {
        Self::Refused(Refusal::new(code, message))
    }

    /// Shorthand for a fence loss.
    pub fn conflict(
        code: &'static str,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self::Conflict { code, expected: expected.into(), actual: actual.into() }
    }

    /// The machine code, where the variant carries one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Refused(refusal) => Some(refusal.code),
            Self::Conflict { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Why a store error happened, where the variant carries a reason.
    ///
    /// This is the half the caller could never see. The store name alone
    /// answers "which file" and never "what went wrong", so `corrupt store:
    /// activity` was the entire diagnosis available to an operator for a lock
    /// timeout, a full disk, and a broken invariant alike.
    #[must_use]
    pub fn cause(&self) -> Option<&str> {
        match self {
            Self::StoreFailure { cause, .. } | Self::Corrupt { cause, .. } => Some(cause),
            _ => None,
        }
    }

    /// The wire discriminant, used by `chiefd-api` and by corpus matching.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Refused(_) => "Refused",
            Self::Conflict { .. } => "Conflict",
            Self::Busy(_) => "Busy",
            Self::StoreFailure { .. } => "StoreFailure",
            Self::Corrupt { .. } => "Corrupt",
            Self::Absent { .. } => "Absent",
            Self::Unavailable { .. } => "Unavailable",
        }
    }
}

impl From<Refusal> for ChiefdError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_carries_code_and_routes_for_the_corpus_matcher() {
        let refusal = Refusal::new("ack-retry-exhausted", "already retried once")
            .with_routes(["assignment.result".to_string()]);
        assert_eq!(refusal.code, "ack-retry-exhausted");
        assert_eq!(refusal.legal_routes, vec!["assignment.result".to_string()]);
    }

    #[test]
    fn kind_names_match_the_closed_taxonomy() {
        assert_eq!(ChiefdError::refused("x", "y").kind(), "Refused");
        assert_eq!(ChiefdError::conflict("fence-mismatch", "3", "4").kind(), "Conflict");
        assert_eq!(store_failure("launch-intent", "x").kind(), "StoreFailure");
        assert_eq!(corrupt_store("launch-intent", "x").kind(), "Corrupt");
        assert_eq!(ChiefdError::Unavailable { reason: "starting" }.kind(), "Unavailable");
    }

    /// The two store kinds are distinct in name, in rendering and in what they
    /// claim. A rename that left them rendering the same sentence would let
    /// `corrupt store: …` keep reaching operators from a lock timeout, which
    /// is the entire defect this split exists to end.
    #[test]
    fn the_two_store_kinds_say_different_things() {
        let generic =
            ChiefdError::StoreFailure { store: "activity", cause: "DatabaseBusy".to_owned() };
        let decoded =
            ChiefdError::Corrupt { store: "activity", cause: "expected value".to_owned() };
        assert_eq!(generic.to_string(), "store failure: activity: DatabaseBusy");
        assert_eq!(decoded.to_string(), "corrupt store: activity: expected value");
        assert_ne!(generic.kind(), decoded.kind());
        assert!(!generic.to_string().contains("corrupt"));
    }

    #[test]
    fn refusal_converts_into_the_taxonomy_error() {
        let err: ChiefdError = Refusal::new("already-exists", "taken").into();
        assert_eq!(err.kind(), "Refused");
    }

    /// A SQLite error is a store failure, NOT corruption. `rusqlite` is how a
    /// lock timeout, a full disk and a constraint violation all arrive, and
    /// none of the three is damaged data.
    #[test]
    fn store_failure_flattens_a_sql_error_without_claiming_damage() {
        let err = store_failure("activity", rusqlite::Error::QueryReturnedNoRows);
        assert!(matches!(err, ChiefdError::StoreFailure { store: "activity", .. }));
        assert_eq!(err.kind(), "StoreFailure");
        // The SQLite variant reaches the VALUE, not just stderr.
        assert_eq!(err.cause(), Some("QueryReturnedNoRows"));
        assert!(err.to_string().contains("QueryReturnedNoRows"), "{err}");
    }

    /// The cause the SQLite result code lives in must survive into the value.
    /// `Display` on `rusqlite::Error` renders "database is locked" with no
    /// code at all, and telling `SQLITE_BUSY` (contention) from `SQLITE_IOERR`
    /// (a full disk) is the question this label has never been able to answer.
    #[test]
    fn the_carried_cause_distinguishes_contention_from_a_full_disk() {
        let busy = store_failure(
            "activity",
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(5), // SQLITE_BUSY
                Some("database is locked".to_owned()),
            ),
        );
        let cause = busy.cause().expect("a store failure carries its cause");
        assert!(cause.contains("DatabaseBusy"), "{cause}");
        assert!(!cause.contains("SystemIoFailure"), "{cause}");
    }

    /// The narrow door still exists, and it still says `Corrupt`, because a
    /// body that would not decode IS damaged and an operator should be told so.
    #[test]
    fn corrupt_store_is_reserved_for_a_decode_failure() {
        let decode = serde_json::from_str::<u32>("{").expect_err("invalid JSON");
        let err = corrupt_store("activity", decode);
        assert!(matches!(err, ChiefdError::Corrupt { store: "activity", .. }));
        assert_eq!(err.kind(), "Corrupt");
        // The serde error names what it saw and where; that is the whole value
        // of knowing a body did not decode.
        let cause = err.cause().expect("a decode failure carries its cause");
        assert!(!cause.is_empty(), "an empty cause is the defect, not the fix");
        assert!(err.to_string().contains(cause), "{err}");
    }

    /// The one test that observes the ACTUAL SUBJECT. The two above assert the
    /// return value and the formatting of `rusqlite::Error` — deleting the
    /// `eprintln!` entirely leaves both green, which is exactly the vacuous
    /// shape this programme has been catching all day. Preserving the cause is
    /// a side effect on stderr, so it can only be observed from a child
    /// process: this test re-runs itself with a marker variable set and asserts
    /// on what the child actually printed.
    #[test]
    fn the_preserved_cause_actually_reaches_stderr() {
        const MARKER: &str = "CHIEFD_CORRUPT_STORE_ECHO_CHILD";
        if std::env::var(MARKER).is_ok() {
            let busy = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(5), // SQLITE_BUSY
                Some("database is locked".to_owned()),
            );
            let _ = store_failure("activity", busy);
            return;
        }
        let exe = std::env::current_exe().expect("the test binary");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "error::tests::the_preserved_cause_actually_reaches_stderr",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .output()
            .expect("re-running the test binary as a child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("[activity] store error:"),
            "no store line in child stderr: {stderr}"
        );
        assert!(
            stderr.contains("DatabaseBusy"),
            "the SQLite result code must survive to stderr: {stderr}"
        );
    }

    /// The reason the bound is `Debug`: the rendered cause must carry the
    /// SQLite result code, because distinguishing `SQLITE_BUSY` (contention)
    /// from `SQLITE_IOERR` (a full disk) is the question `corrupt store: …`
    /// has never been able to answer. `Display` alone renders "database is
    /// locked" with no code at all.
    #[test]
    fn the_rendered_cause_carries_the_sqlite_result_code() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(5), // SQLITE_BUSY
            Some("database is locked".to_owned()),
        );
        let rendered = format!("{busy:?}");
        assert!(rendered.contains("DatabaseBusy"), "{rendered}");
        // ...and the code is exactly what Display drops on the floor.
        assert!(!format!("{busy}").contains("DatabaseBusy"), "{busy}");
    }
}

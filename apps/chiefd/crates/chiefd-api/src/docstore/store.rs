//! The typed chiefd infrastructure store — normalized semantic operations
//! with SQL built **server-side**.
//!
//! Every method here is a semantic operation (typed row publishing), never a
//! SQL string supplied by a caller.
//!
//! Clock authority stays with the caller. `updated_at` and `now` are all
//! passed IN, exactly as the TypeScript store computes them today
//! (`Date.now`, injected in tests). The server never reads a clock, so
//! repointing the transport does not move where time is decided.
//!
//! #830 (E8-S6c): the durable TTL lease this module used to serve over
//! `/v1/locks/*` (`org_locks`, `LockRow`, `lock_acquire`/`lock_renew`/
//! `lock_release`/`lock_break`/`lock_list`, and the `#99` holder-liveness
//! helpers that existed only to let `lock_acquire` reclaim a dead holder's
//! lease) is deleted. Ruling D19 names a TTL lease a lock in so many words;
//! chiefd's single-writer queue is the only mutual exclusion left. No
//! replacement, no stub route, no `DROP TABLE` — old databases are
//! abandoned, not converted (D0/D24-F27).

use std::sync::Arc;

use super::engine::{DocEngine, OpenError, WriteFailure};
use super::feed::ChangeFeed;

/// Why a typed store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A write failed (SQL error or a dead writer).
    #[error(transparent)]
    Write(#[from] WriteFailure),
    /// A read failed; the string is SQLite's message.
    #[error("read failed: {0}")]
    Query(String),
    /// A row came back in a shape the typed layer did not expect. Unreachable
    /// unless the schema drifted underneath the daemon — surfaced rather than
    /// unwrapped so a corrupt row is a loud refusal, not a panic.
    #[error("unexpected row shape: {0}")]
    RowShape(String),
}

/// The typed store over one `org.sqlite` (or a per-company database).
pub struct DocStore {
    /// `pub(crate)` so a sibling module can issue its own multi-statement
    /// transactions directly, for mutations that do not fit the
    /// single-statement shape every method above does.
    pub(crate) engine: DocEngine,
    /// The change-feed: emitted from THIS layer, after the outcome check,
    /// because `rows_affected == 1` (CAS win vs. loss, insert vs. no-op) is
    /// decided here, not in [`DocEngine`]. `pub(crate)` for the same reason
    /// as `engine` — a sibling module may publish its own synthetic events,
    /// which commit through
    /// `DocEngine::exec_interactive` and so never pass through the store
    /// methods below. See `docstore::feed` for the wire-event contract.
    ///
    /// `Arc`-wrapped (#376) so a caller can share this exact feed instance
    /// with something outside `DocStore` entirely — namely `chiefd-core`'s
    /// `CompanyDb` writer actor, whose own duty commits publish onto the
    /// SAME feed via [`open_with_feed`](DocStore::open_with_feed) /
    /// [`docstore::bind_with_feed`](super::bind_with_feed), so one
    /// `/v1/docs/watch` subscriber sees both this store's writes and
    /// `CompanyDb`'s.
    pub(crate) feed: Arc<ChangeFeed>,
}

impl DocStore {
    /// Open the backing engine with its own, freshly minted change-feed.
    ///
    /// # Errors
    /// [`OpenError`] when the database cannot be opened.
    pub fn open(db_path: &str, read_pool: usize) -> Result<Self, OpenError> {
        Self::open_with_feed(db_path, read_pool, Arc::new(ChangeFeed::new()))
    }

    /// Open the backing engine, publishing onto an externally-owned
    /// `feed` rather than minting a fresh one (#376).
    ///
    /// Used when another writer into the SAME physical database — concretely,
    /// `chiefd-core`'s `CompanyDb` duty commits against the shared
    /// `org.sqlite`/`chief.db` file (`resolve_company_db_path` in `chiefd`'s
    /// `run.rs`) — must publish onto the identical `ChangeFeed` this store's
    /// own writes use, so a `/v1/docs/watch` subscriber learns about either
    /// writer's commits from one feed instead of being blind to one of them.
    ///
    /// # Errors
    /// [`OpenError`] when the database cannot be opened.
    pub fn open_with_feed(
        db_path: &str,
        read_pool: usize,
        feed: Arc<ChangeFeed>,
    ) -> Result<Self, OpenError> {
        Ok(Self { engine: DocEngine::open(db_path, read_pool)?, feed })
    }

    /// The backing engine, for `/health` counters.
    #[must_use]
    pub fn engine(&self) -> &DocEngine {
        &self.engine
    }

    /// The change-feed, for the SSE endpoint (SSE-B) to subscribe/replay
    /// against. See `docstore::feed` for the event contract.
    #[must_use]
    pub fn feed(&self) -> &ChangeFeed {
        &self.feed
    }

    /// Create the schema if absent — the typed form of `ensureSchema()`.
    ///
    /// # Errors
    /// [`StoreError::Write`] on failure.
    pub async fn ensure_schema(&self) -> Result<(), StoreError> {
        // Cross-producer exactly-once event markers (org-data-normalization P0,
        // #33). DocStore-direct so bare-dir / no-live-company writes work; the
        // DDL is the single-source `EVENT_ONCE_MARKERS_DDL` (table + index, two
        // statements) so it never drifts from COMPANY_SCHEMA_SQL.
        self.engine
            .exec_interactive(|tx| {
                tx.execute_batch(chiefd_core::schema::EVENT_ONCE_MARKERS_DDL)
                    .map_err(|e| StoreError::Write(WriteFailure::Sql(e.to_string())))
            })
            .await?;
        Ok(())
    }

    /// The `/health` verdict: `Ok(())` ⇒ writer alive AND schema present.
    ///
    /// # Errors
    /// The `503` cause string.
    pub async fn health_probe(&self) -> Result<(), String> {
        self.engine.health_probe().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_probe_is_ok_only_once_the_schema_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("org.sqlite").display().to_string();
        let store = DocStore::open(&path, 2).expect("open");
        assert!(store.health_probe().await.is_err(), "a schema-less file must not report healthy");
        store.ensure_schema().await.expect("schema");
        assert!(
            store.health_probe().await.is_ok(),
            "healthy once normalized infrastructure exists"
        );
    }
}

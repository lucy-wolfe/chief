//! The single-writer SQLite engine under chiefd's surviving `org_documents`
//! surface.
//!
//! This is **not** `legacy_sql::engine`. That engine backs the raw-SQL
//! passthrough (`/exec`, `/query`, `/batch`) that speaks write-db's protocol
//! verbatim, and it is deleted together with `legacy_sql` and the write-db
//! service at Phase B. This engine backs the
//! **typed** `org_documents` surface that *survives* that deletion, so it is a
//! deliberately self-contained copy of the single-writer discipline rather than
//! a dependency on a module that dies. The apparent duplication is transient:
//! after Phase B this is the only `org.sqlite` engine left in the tree.
//!
//! The discipline is the whole point (same as write-db and `legacy_sql`): ONE
//! connection ever performs writes, fed by a FIFO channel, so SQLite never
//! raises `SQLITE_BUSY` on the write path and `busy_retries` stays 0 by
//! construction. A `/health` probe rides the same FIFO as writes, so it cannot
//! answer healthy while the write queue is jammed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::types::{Value, ValueRef};
use rusqlite::Connection;
use serde::Serialize;
use tokio::sync::{oneshot, Semaphore};

/// A single write statement plus its positional parameters.
#[derive(Clone, Debug)]
pub struct Statement {
    /// The SQL text. Built server-side by [`super::store`], never accepted from
    /// a client — that is the difference from the legacy passthrough.
    pub sql: String,
    /// Positional parameters for `?1`, `?2`, …
    pub params: Vec<serde_json::Value>,
}

/// The outcome of one write.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExecOutcome {
    /// `changes()` after the statement — the CAS / lock family reads this to
    /// decide whether the optimistic write applied.
    pub rows_affected: usize,
    /// `last_insert_rowid()` on the writer connection.
    pub last_insert_id: i64,
}

/// A read result: column names and JSON-coerced row-major cells.
#[derive(Debug, Clone, Serialize)]
pub struct Rows {
    /// Column names in selection order.
    pub columns: Vec<String>,
    /// Row-major cells.
    pub rows: Vec<Vec<serde_json::Value>>,
}

/// The counters `/v1/docs/health` publishes.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Successful writes.
    pub writes_total: AtomicU64,
    /// Reads, counted on entry.
    pub queries_total: AtomicU64,
    /// Writer-path busy/locked errors. Expected to remain 0 forever — a
    /// non-zero value means a second writer is touching the file.
    pub busy_retries: AtomicU64,
}

struct WriteJob {
    statement: Statement,
    reply: oneshot::Sender<Result<ExecOutcome, String>>,
}

/// Several statements that must apply atomically — all or nothing — on the
/// SAME connection. Added for the task model (#240): one task mutation is
/// the row change + an audit row + zero-or-more notification rows + parent
/// re-derivation, and a partial write (e.g. the audit row lands but the
/// process dies before the notification row) would corrupt the audit trail's
/// completeness guarantee. `org_documents` never needed this — every one of
/// its writes is already a single statement.
struct TransactionJob {
    statements: Vec<Statement>,
    reply: oneshot::Sender<Result<Vec<ExecOutcome>, String>>,
}

/// A boxed, type-erased "run this with full read+write access inside one
/// transaction" job. `WriterMsg` cannot be generic over a per-call result
/// type, so the closure captures its own reply channel and sends into it
/// itself, rather than the writer loop forwarding a typed return value.
type InteractiveJob = Box<dyn FnOnce(&Connection) + Send>;

/// Writes and health probes share ONE channel so a probe proves the same thing
/// a write does: the thread is alive and not wedged behind the FIFO.
enum WriterMsg {
    Job(WriteJob),
    Transaction(TransactionJob),
    Interactive(InteractiveJob),
    HealthProbe(oneshot::Sender<Result<(), String>>),
}

/// How long `/health` waits for the writer to answer before calling it wedged.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Assert the durable infrastructure schema exists. The generic document table
/// is intentionally absent: normalized product state lives in typed tables,
/// while this engine owns the task infrastructure used by the HTTP surface.
/// #830: `org_locks` was the other table this checked; deleted with the rest
/// of the TTL lease (D19/D0 — no replacement, no stub, no `DROP TABLE` on an
/// abandoned old database's leftover `org_locks` either).
fn schema_present(conn: &Connection) -> Result<(), String> {
    match conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = 'event_once_markers'",
        [],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(1) => Ok(()),
        Ok(found) => {
            Err(format!("schema-missing: normalized infrastructure incomplete ({found}/1 tables)"))
        }
        Err(e) => Err(format!("schema-check-failed: {e}")),
    }
}

/// Failure opening the database.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// A connection could not be opened, or its pragmas could not be applied.
    #[error("org.sqlite at {path}: {source}")]
    Sqlite {
        /// The database file chiefd was asked to own.
        path: String,
        /// Underlying rusqlite failure.
        #[source]
        source: rusqlite::Error,
    },
    /// The writer thread could not be spawned.
    #[error("spawn org_documents writer thread: {0}")]
    Spawn(#[source] std::io::Error),
}

/// How a write failed. `docstore::route_error` owns the status each variant is
/// answered with; nothing here decides one.
#[derive(Debug, thiserror::Error)]
pub enum WriteFailure {
    /// SQLite refused the statement; the string is its message.
    #[error("{0}")]
    Sql(String),
    /// The writer channel is closed — the thread is gone.
    #[error("writer down")]
    WriterDown,
    /// The writer accepted the job and then dropped the reply channel.
    #[error("writer dropped")]
    WriterDropped,
}

/// One writer thread, a small read pool, and the counters.
pub struct DocEngine {
    writer_tx: Sender<WriterMsg>,
    read_pool: ReadPool,
    metrics: Arc<Metrics>,
    started: Instant,
}

impl DocEngine {
    /// Open `db_path` with `read_pool` read-only connections and start the
    /// writer thread.
    ///
    /// # Errors
    /// [`OpenError`] when a connection cannot be opened, a pragma cannot be
    /// applied, or the writer thread cannot be spawned.
    pub fn open(db_path: &str, read_pool: usize) -> Result<Self, OpenError> {
        let metrics = Arc::new(Metrics::default());
        let pool = ReadPool::new(db_path, read_pool.max(1))?;
        let writer_tx = spawn_writer(db_path.to_string(), Arc::clone(&metrics))?;
        Ok(Self { writer_tx, read_pool: pool, metrics, started: Instant::now() })
    }

    /// Counters for `/health`.
    #[must_use]
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Seconds since [`DocEngine::open`].
    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Probe the writer and the schema for `/health`.
    ///
    /// `Ok(())` — the writer answered AND `org_documents` exists (→ `200 ok`).
    /// `Err(cause)` — a `503` cause: `writer-down`, `writer-dropped`,
    /// `writer-unresponsive`, or `schema-missing: …`.
    ///
    /// # Errors
    /// The `503` cause string when the writer is unreachable or the schema is
    /// absent.
    pub async fn health_probe(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        if self.writer_tx.send(WriterMsg::HealthProbe(reply)).is_err() {
            return Err("writer-down".to_string());
        }
        match tokio::time::timeout(HEALTH_PROBE_TIMEOUT, rx).await {
            Ok(Ok(inner)) => inner,
            Ok(Err(_)) => Err("writer-dropped".to_string()),
            Err(_) => Err("writer-unresponsive".to_string()),
        }
    }

    /// Submit one statement to the single writer.
    ///
    /// # Errors
    /// [`WriteFailure`] when the writer is gone or SQLite refuses the statement.
    pub async fn exec(&self, statement: Statement) -> Result<ExecOutcome, WriteFailure> {
        let (reply, rx) = oneshot::channel();
        self.writer_tx
            .send(WriterMsg::Job(WriteJob { statement, reply }))
            .map_err(|_| WriteFailure::WriterDown)?;
        match rx.await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(message)) => Err(WriteFailure::Sql(message)),
            Err(_) => Err(WriteFailure::WriterDropped),
        }
    }

    /// Submit several statements to the single writer as ONE atomic
    /// transaction: either every statement applies, in order, or (on the
    /// first failure) none of them do — the writer thread rolls back and
    /// reports the failing statement's message. Outcomes are returned in
    /// the same order as `statements`.
    ///
    /// # Errors
    /// [`WriteFailure`] when the writer is gone, or SQLite refuses any
    /// statement (the whole transaction is rolled back in that case).
    pub async fn exec_transaction(
        &self,
        statements: Vec<Statement>,
    ) -> Result<Vec<ExecOutcome>, WriteFailure> {
        let (reply, rx) = oneshot::channel();
        self.writer_tx
            .send(WriterMsg::Transaction(TransactionJob { statements, reply }))
            .map_err(|_| WriteFailure::WriterDown)?;
        match rx.await {
            Ok(Ok(outcomes)) => Ok(outcomes),
            Ok(Err(message)) => Err(WriteFailure::Sql(message)),
            Err(_) => Err(WriteFailure::WriterDropped),
        }
    }

    /// Run `f` inside ONE atomic transaction on the single writer connection,
    /// with full read+write access — for logic that must branch on data it
    /// just read before deciding what to write next. The task model's parent-
    /// status derivation is the motivating case: read a parent's children,
    /// decide the derived status in Rust, conditionally write an audit row
    /// and notification, and walk to the grandparent — all of which must see
    /// a consistent snapshot and commit as one unit, which `exec_transaction`
    /// cannot express (it runs a fixed, pre-built list of statements with no
    /// Rust logic in between).
    ///
    /// `f` receives an ordinary `rusqlite::Transaction`, not the `Statement`/
    /// JSON-param plumbing the rest of this engine uses for client-facing
    /// operations — `f` is Rust code chiefd itself wrote (`docstore::tasks`),
    /// never a client-supplied string, so the typed-vs-raw-SQL distinction
    /// this engine otherwise enforces does not apply to it. `f` returning
    /// `Err` rolls the whole transaction back (nothing `f` wrote survives);
    /// returning `Ok` commits.
    ///
    /// Generic over the caller's own error type `E` (e.g. `docstore::tasks`'s
    /// `TaskError`), not just [`WriteFailure`]: `f`'s domain refusals (illegal
    /// transition, unknown parent, a cycle, …) come back as themselves, with
    /// no string-encoding/decoding round trip, as long as `E: From<WriteFailure>`
    /// (so this method can still report writer-down/dropped/SQL failures in
    /// the caller's own error type).
    ///
    /// # Errors
    /// `E::from(WriteFailure::…)` when the writer is gone or the commit
    /// fails; whatever `f` itself returns otherwise.
    pub async fn exec_interactive<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static + From<WriteFailure>,
    {
        let (reply, rx) = oneshot::channel::<Result<T, E>>();
        let job: InteractiveJob = Box::new(move |conn: &Connection| {
            let result: Result<T, E> = (|| {
                let tx = conn
                    .unchecked_transaction()
                    .map_err(|e| E::from(WriteFailure::Sql(e.to_string())))?;
                let value = f(&tx)?;
                tx.commit().map_err(|e| E::from(WriteFailure::Sql(e.to_string())))?;
                Ok(value)
            })();
            let _ = reply.send(result);
        });
        self.writer_tx
            .send(WriterMsg::Interactive(job))
            .map_err(|_| E::from(WriteFailure::WriterDown))?;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(E::from(WriteFailure::WriterDropped)),
        }
    }

    /// Run a read on a pooled `query_only` connection.
    ///
    /// # Errors
    /// SQLite's message when the statement fails.
    pub async fn query(&self, sql: String, params: Vec<serde_json::Value>) -> Result<Rows, String> {
        self.metrics.queries_total.fetch_add(1, Ordering::Relaxed);
        self.read_pool.query(sql, params).await
    }

    /// A dedicated single-pass point read for a `(TEXT, INTEGER)` row —
    /// exactly the `(blob, generation)` shape `org_documents` reads need
    /// (od:chiefd-cpu #310). Unlike [`DocEngine::query`], this never builds a
    /// `serde_json::Value` intermediate: the text column comes straight off
    /// the rusqlite row into an owned `String` (rusqlite's `Row::get::<_,
    /// String>` — ONE allocation, not [`run_query`]'s `value_ref_to_json`
    /// allocation followed by [`super::store::cell_str`]'s second `.to_string()`
    /// clone of the same bytes). `None` when the row is absent; `Some` rows
    /// beyond the first are impossible for a `(slug, store)` primary-key
    /// lookup and are not consumed.
    ///
    /// # Errors
    /// SQLite's message when the statement fails or the row is not the
    /// expected `(TEXT, INTEGER)` shape.
    pub async fn query_text_and_i64(
        &self,
        sql: String,
        params: Vec<serde_json::Value>,
    ) -> Result<Option<(String, i64)>, String> {
        self.metrics.queries_total.fetch_add(1, Ordering::Relaxed);
        self.read_pool.query_text_and_i64(sql, params).await
    }
}

// ---------------------------------------------------------------------------
// Value coercion — byte-compatible with write-db / legacy_sql, because the
// TypeScript client's round-trip bytes depend on it (JSON containers stored as
// their text form, locks passing integers, etc.).
// ---------------------------------------------------------------------------

fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => {
            n.as_i64().map_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0)), Value::Integer)
        }
        serde_json::Value::String(s) => Value::Text(s.clone()),
        other => Value::Text(other.to_string()),
    }
}

fn value_ref_to_json(v: ValueRef<'_>) -> serde_json::Value {
    match v {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::from(i),
        ValueRef::Real(f) => serde_json::Value::from(f),
        ValueRef::Text(t) => serde_json::Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => serde_json::Value::from(format!("blob:{}bytes", b.len())),
    }
}

fn apply_writer_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    Ok(())
}

fn apply_reader_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(())
}

fn exec_statement(conn: &Connection, stmt: &Statement) -> rusqlite::Result<ExecOutcome> {
    let params: Vec<Value> = stmt.params.iter().map(json_to_value).collect();
    let rows_affected = conn.execute(&stmt.sql, rusqlite::params_from_iter(params.iter()))?;
    Ok(ExecOutcome { rows_affected, last_insert_id: conn.last_insert_rowid() })
}

fn spawn_writer(db_path: String, metrics: Arc<Metrics>) -> Result<Sender<WriterMsg>, OpenError> {
    // Seam note (clippy.toml): `chiefd_core::store` owns every connection to a
    // per-company `chief.db`. This is the shared legacy `org.sqlite`, whose
    // schema and lifetime belong to the TypeScript store until the SQL-migration
    // waves retire it — it must NOT be a `chiefd_core` ledger, so it is opened
    // here directly, exactly as `legacy_sql::engine` does and for the same
    // reason.
    #[allow(clippy::disallowed_methods)] // shared org.sqlite is not a chiefd ledger — see above
    let conn = Connection::open(&db_path)
        .map_err(|source| OpenError::Sqlite { path: db_path.clone(), source })?;
    apply_writer_pragmas(&conn)
        .map_err(|source| OpenError::Sqlite { path: db_path.clone(), source })?;

    let (tx, rx) = channel::<WriterMsg>();
    std::thread::Builder::new()
        .name("org-documents-writer".into())
        .spawn(move || {
            while let Ok(msg) = rx.recv() {
                match msg {
                    WriterMsg::Job(job) => {
                        let result = exec_statement(&conn, &job.statement).map_err(|e| {
                            if matches!(
                                e.sqlite_error_code(),
                                Some(rusqlite::ErrorCode::DatabaseBusy)
                                    | Some(rusqlite::ErrorCode::DatabaseLocked)
                            ) {
                                metrics.busy_retries.fetch_add(1, Ordering::Relaxed);
                            }
                            e.to_string()
                        });
                        if result.is_ok() {
                            metrics.writes_total.fetch_add(1, Ordering::Relaxed);
                        }
                        let _ = job.reply.send(result);
                    }
                    WriterMsg::Transaction(job) => {
                        // `unchecked_transaction` (needs only `&Connection`, not
                        // `&mut`) is safe here specifically because this writer
                        // thread is the ONLY thing that ever touches `conn` — the
                        // single-writer discipline this whole engine exists for.
                        // Dropping `tx` without `commit()` (any `?` below) rolls
                        // back automatically (rusqlite's `Transaction::drop`).
                        let result = (|| -> rusqlite::Result<Vec<ExecOutcome>> {
                            let tx = conn.unchecked_transaction()?;
                            let mut outcomes = Vec::with_capacity(job.statements.len());
                            for statement in &job.statements {
                                outcomes.push(exec_statement(&tx, statement)?);
                            }
                            tx.commit()?;
                            Ok(outcomes)
                        })()
                        .map_err(|e| {
                            if matches!(
                                e.sqlite_error_code(),
                                Some(rusqlite::ErrorCode::DatabaseBusy)
                                    | Some(rusqlite::ErrorCode::DatabaseLocked)
                            ) {
                                metrics.busy_retries.fetch_add(1, Ordering::Relaxed);
                            }
                            e.to_string()
                        });
                        if let Ok(outcomes) = &result {
                            metrics
                                .writes_total
                                .fetch_add(outcomes.len() as u64, Ordering::Relaxed);
                        }
                        let _ = job.reply.send(result);
                    }
                    WriterMsg::Interactive(job) => {
                        job(&conn);
                        metrics.writes_total.fetch_add(1, Ordering::Relaxed);
                    }
                    WriterMsg::HealthProbe(reply) => {
                        let _ = reply.send(schema_present(&conn));
                    }
                }
            }
        })
        .map_err(OpenError::Spawn)?;
    Ok(tx)
}

/// A fixed-size pool of read-only connections behind a semaphore.
#[derive(Clone)]
struct ReadPool {
    conns: Arc<Mutex<Vec<Connection>>>,
    permits: Arc<Semaphore>,
}

impl ReadPool {
    fn new(db_path: &str, size: usize) -> Result<Self, OpenError> {
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            #[allow(clippy::disallowed_methods)] // shared org.sqlite — see spawn_writer
            let c = Connection::open(db_path)
                .map_err(|source| OpenError::Sqlite { path: db_path.to_string(), source })?;
            apply_reader_pragmas(&c)
                .map_err(|source| OpenError::Sqlite { path: db_path.to_string(), source })?;
            conns.push(c);
        }
        Ok(Self { conns: Arc::new(Mutex::new(conns)), permits: Arc::new(Semaphore::new(size)) })
    }

    async fn query(&self, sql: String, params: Vec<serde_json::Value>) -> Result<Rows, String> {
        self.checkout(move |conn| run_query(conn, &sql, &params)).await
    }

    async fn query_text_and_i64(
        &self,
        sql: String,
        params: Vec<serde_json::Value>,
    ) -> Result<Option<(String, i64)>, String> {
        self.checkout(move |conn| run_text_and_i64_query(conn, &sql, &params)).await
    }

    /// Check out a pooled connection, run `f` against it on a blocking
    /// thread, and return it to the pool — the pool-checkout boilerplate
    /// every read shares, parameterized over what the read itself does.
    async fn checkout<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "read pool closed".to_string())?;
        let conns = Arc::clone(&self.conns);
        let out = tokio::task::spawn_blocking(move || {
            // A poisoned mutex means a reader panicked mid-checkout; the vector
            // is only pushed/popped, so recover the guard rather than taking the
            // daemon down.
            let taken = match conns.lock() {
                Ok(mut g) => g.pop(),
                Err(poisoned) => poisoned.into_inner().pop(),
            };
            let conn = taken.ok_or_else(|| "read pool exhausted".to_string())?;
            let result = f(&conn);
            match conns.lock() {
                Ok(mut g) => g.push(conn),
                Err(poisoned) => poisoned.into_inner().push(conn),
            }
            result.map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?;
        drop(permit);
        out
    }
}

fn run_query(conn: &Connection, sql: &str, params: &[serde_json::Value]) -> rusqlite::Result<Rows> {
    let sql_params: Vec<Value> = params.iter().map(json_to_value).collect();
    // `prepare_cached`, not `prepare` (od:chiefd-cpu #310 item 1): every SQL
    // string reaching this function is a fixed, server-built constant from
    // `store.rs` — never client-supplied — so caching by exact SQL text is
    // unconditionally safe (no cache-key explosion, unlike a raw-SQL
    // passthrough) and avoids re-parsing the same statement on every call.
    let mut stmt = conn.prepare_cached(sql)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|s| (*s).to_string()).collect();
    let col_count = columns.len();
    let mut rows_out = Vec::new();
    let mut rows = stmt.query(rusqlite::params_from_iter(sql_params.iter()))?;
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(col_count);
        for i in 0..col_count {
            cells.push(value_ref_to_json(row.get_ref(i)?));
        }
        rows_out.push(cells);
    }
    Ok(Rows { columns, rows: rows_out })
}

/// The single-pass point read behind [`DocEngine::query_text_and_i64`]
/// (od:chiefd-cpu #310): SQLite row buffer → owned `String` in one copy, no
/// `serde_json::Value` intermediate and no second `.to_string()` clone at the
/// call site. `sql` must select exactly one `TEXT` column then one `INTEGER`
/// column, in that order — true of every caller today (`READ_ROW`,
/// `READ_GENERATION`'s generation-only shape does NOT use this, it stays on
/// the generic path since it has no text column to special-case).
fn run_text_and_i64_query(
    conn: &Connection,
    sql: &str,
    params: &[serde_json::Value],
) -> rusqlite::Result<Option<(String, i64)>> {
    let sql_params: Vec<Value> = params.iter().map(json_to_value).collect();
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(sql_params.iter()))?;
    match rows.next()? {
        Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
        None => Ok(None),
    }
}

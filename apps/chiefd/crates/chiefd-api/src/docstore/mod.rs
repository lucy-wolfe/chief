//! The **surviving** `org_documents` surface — chiefd's typed document store.
//!
//! # Why this exists, and what it is not
//!
//! The operator: "only one daemon … EVERYTHING into chiefd." At the end state there is
//! no write-db service and no `legacy_sql` passthrough — both are deleted
//! together at Phase B. Something must still
//! serve the `org_documents` contract the nineteen TypeScript stores depend on,
//! because moving those stores off the contract is the *SQL-migration* project,
//! not this one, and Phase A may not touch `src/organization`. That something is
//! this module.
//!
//! `legacy_sql` is a raw-SQL passthrough: it accepts (allowlisted) SQL strings
//! and speaks write-db's `/exec` `/query` `/batch` protocol verbatim, which is
//! precisely why it is throwaway. This surface is the opposite: a fixed set of
//! **typed** operations (`/v1/docs/read`, `/v1/docs/cas`, …) that chiefd owns
//! end to end. chiefd is the authority on the contract, not a SQL pipe, so
//! this is the one surface that survives.
//!
//! # The contract, unchanged
//!
//! Document CRUD keyed `(slug, store)`; generation compare-and-swap; and the
//! insert-if-absent / replace-if-generation split. The
//! SQL and the semantics are transcribed byte-for-byte from
//! `src/organization/org-durable-store.ts` — see [`store`] — so a repointed
//! client sees identical behaviour.
//!
//! # The Phase-B repoint is minimal by construction
//!
//! The route set maps one-to-one onto the public methods of
//! `SqliteDurableStore` / `DurableLock`. At Phase B, only
//! `org-durable-store.ts`'s private transport helpers change to call these
//! routes; the public store API is untouched, so none of the nineteen stores
//! move. The exact recipe is recorded in the design record.

mod bench_completion;
mod caller_auth;
mod company_tree;
mod desired;
mod disclosure_fence;
mod engine;
pub mod feed;
pub(crate) mod org_slice;
mod roster;
pub mod route_error;
mod router;
mod runtime_routes;
mod store;

pub use bench_completion::BenchCompletionRegistry;
pub use engine::{DocEngine, OpenError};
pub use feed::{ChangeFeed, Replay, WatchEvent};
pub use router::{
    request_log_level, router, router_with_auth, router_with_heartbeat_interval,
    router_with_live_resolver, router_with_supervision_live, AgentHomeRoot, LiveResolutionMode,
    SupervisionLiveResolver, SupervisionLiveSource,
};
pub use store::{DocStore, StoreError};

/// Default read-pool size, matching write-db's and `legacy_sql`'s default.
pub const DEFAULT_READ_POOL: usize = 8;

/// Default maximum request body. Cobalt's supervision ledger is 4.4 MB and its
/// activity ledger 2.3 MB, so axum's 2 MB default is an outage; this is
/// deliberately enormous.
pub const DEFAULT_MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// How the store surface is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Address to bind. During migration this is a loopback TCP address; at the
    /// end state the daemon mounts [`router`] on its own listener instead.
    pub bind: String,
    /// The `org.sqlite` file. There is no default: serving the wrong file would
    /// look healthy and lose every document.
    pub db_path: String,
    /// Read-only connection count.
    pub read_pool: usize,
    /// Maximum accepted request body in bytes.
    pub max_body_bytes: usize,
}

/// The store surface was asked to start without a database path.
#[derive(Debug, thiserror::Error)]
#[error(
    "the chiefd store surface needs a database path: set CHIEFD_STORE_DB_PATH to the org.sqlite that \
     holds the org_documents contract"
)]
pub struct MissingDbPath;

impl Config {
    /// Resolve configuration from an injected environment lookup (injected, not
    /// read from `std::env`, so precedence is testable without mutating process
    /// state under a parallel runner).
    ///
    /// # Errors
    /// [`MissingDbPath`] when `CHIEFD_STORE_DB_PATH` is unset — a daemon that
    /// silently creates an empty database and reports `ok` is how a cutover
    /// "succeeds" while every document is gone.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self, MissingDbPath> {
        let db_path = var("CHIEFD_STORE_DB_PATH").ok_or(MissingDbPath)?;
        Ok(Self {
            bind: var("CHIEFD_STORE_BIND").unwrap_or_else(|| "127.0.0.1:8792".to_string()),
            db_path,
            read_pool: var("CHIEFD_STORE_READ_POOL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_READ_POOL),
            max_body_bytes: var("CHIEFD_STORE_MAX_BODY_BYTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_BODY_BYTES),
        })
    }

    /// Build a [`Config`] from an EXPLICIT `db_path` — never from
    /// `CHIEFD_STORE_DB_PATH`, even when it is present in the injected
    /// environment — while still honouring `CHIEFD_STORE_BIND`,
    /// `CHIEFD_STORE_READ_POOL` and `CHIEFD_STORE_MAX_BODY_BYTES` from it.
    ///
    /// This is `chiefd run`'s entry point as of E10-S2 (#763): the docstore
    /// surface mounts on the SAME per-company file the duty scheduler's
    /// `CompanyDb` just opened
    /// (`company_db_target::resolve`/`company_db_target::open`), never a
    /// value read from the environment. `from_env` above is untouched and
    /// stays `chiefd docstore-only`'s entry point, which is deliberately
    /// still multi-company and still reads `CHIEFD_STORE_DB_PATH`.
    #[must_use]
    pub fn from_env_with_db_path(var: impl Fn(&str) -> Option<String>, db_path: String) -> Self {
        Self {
            bind: var("CHIEFD_STORE_BIND").unwrap_or_else(|| "127.0.0.1:8792".to_string()),
            db_path,
            read_pool: var("CHIEFD_STORE_READ_POOL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_READ_POOL),
            max_body_bytes: var("CHIEFD_STORE_MAX_BODY_BYTES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_BODY_BYTES),
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn from_env_with_db_path_uses_the_passed_path_even_when_chiefd_store_db_path_is_present_in_the_environment(
    ) {
        let env = |key: &str| match key {
            "CHIEFD_STORE_DB_PATH" => Some("/decoy/should-be-ignored.sqlite".to_string()),
            "CHIEFD_STORE_BIND" => Some("127.0.0.1:9999".to_string()),
            "CHIEFD_STORE_READ_POOL" => Some("4".to_string()),
            "CHIEFD_STORE_MAX_BODY_BYTES" => Some("1024".to_string()),
            _ => None,
        };
        let config =
            Config::from_env_with_db_path(env, "/data/orgs/.northstar.chief.db".to_string());
        assert_eq!(config.db_path, "/data/orgs/.northstar.chief.db");
        assert_eq!(config.bind, "127.0.0.1:9999");
        assert_eq!(config.read_pool, 4);
        assert_eq!(config.max_body_bytes, 1024);
    }

    #[test]
    fn from_env_with_db_path_fills_in_defaults_when_the_environment_has_none_of_the_optional_vars()
    {
        let config =
            Config::from_env_with_db_path(|_| None, "/data/orgs/.northstar.chief.db".to_string());
        assert_eq!(config.db_path, "/data/orgs/.northstar.chief.db");
        assert_eq!(config.bind, "127.0.0.1:8792");
        assert_eq!(config.read_pool, DEFAULT_READ_POOL);
        assert_eq!(config.max_body_bytes, DEFAULT_MAX_BODY_BYTES);
    }
}

/// The store surface could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The database could not be opened.
    #[error(transparent)]
    Open(#[from] OpenError),
    /// The database opened and bound, but its durable schema could not be
    /// established before the surface began accepting requests.
    #[error("chiefd store surface schema initialization failed: {0}")]
    Schema(#[from] StoreError),
    /// The address could not be bound, or serving failed.
    ///
    /// [`bind_walking`] also uses this variant when it exhausts every
    /// candidate in `[base, base + walk)` — `bind` is then set to the WHOLE
    /// range tried (`"127.0.0.1:8792..127.0.0.1:8855 (64 attempts)"`), not
    /// just the last address, so an operator sees the actual contention
    /// window rather than one arbitrary port; `source` is the last
    /// candidate's own `AddrInUse` error.
    #[error("chiefd store surface on {bind}: {source}")]
    Bind {
        /// Address chiefd tried to own (or the whole walked range).
        bind: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// An opened, bound-but-not-yet-serving store surface: the database is open and
/// the listener already owns its address, but no request is accepted until
/// [`serve_bound`] runs.
///
/// Splitting bind from serve is what lets the one-daemon assembly (`chiefd run`)
/// mount this surface on its OWN `tokio` task under the daemon's shutdown signal
/// — it binds fail-fast (so a taken address refuses the whole daemon, exactly as
/// a standalone [`serve`] would), learns the actual bound address (an ephemeral
/// `:0` in tests), and then drives graceful shutdown itself, rather than the
/// listener outliving the duty tasks it runs beside.
pub struct Bound {
    listener: tokio::net::TcpListener,
    store: std::sync::Arc<DocStore>,
    db_path: String,
    read_pool: usize,
    max_body_bytes: usize,
    /// #372: `Some` only via [`bind_with_feed_and_company`] -- `chiefd run`'s
    /// one-daemon assembly, never the standalone/migration entrypoints.
    supervision_live: Option<router::SupervisionLiveSource>,
    /// Per-request live-source resolver for the multi-company `docstore-only`
    /// test surface (org-data-normalization P0, N8). `None` in production, where
    /// `supervision_live` alone (one company) is authoritative.
    resolver: Option<router::SupervisionLiveResolver>,
    /// agent-auth (P0): the auth runtime that powers the `/v1/auth/*` handlers
    /// AND the verify-middleware. `Some` whenever `chiefd run` attaches it via
    /// [`Bound::with_auth`], and then every non-exempt route requires a bearer;
    /// `None` on the standalone/migration surface, which has no company actor
    /// and therefore no identities to resolve.
    ///
    /// It used to be accompanied by `enforce: bool`, fed from
    /// `CHIEFD_AUTH_ENABLED`, which decided whether a bearer-less request was
    /// refused. That is deleted (A6): a runtime's presence IS the decision, and
    /// there is no second field to disagree with it.
    auth: Option<std::sync::Arc<crate::authn::runtime::AuthRuntime>>,
    /// Process role served by `/v1/docs/runtime`. A launched company sets
    /// `company`; bootstrap/standalone sets `docstore-only`. Generic library
    /// callers leave this absent and therefore do not mount the identity route.
    runtime_identity: Option<RuntimeIdentity>,
    /// The daemon's shutdown-request sender (E7-S3). `Some` mounts
    /// `POST /v1/admin/shutdown`; `None` (generic library callers with no
    /// daemon shutdown watch) leaves the route unmounted — same optionality
    /// as `runtime_identity`.
    shutdown_requester: Option<tokio::sync::watch::Sender<Option<String>>>,
    /// Something to poke on each liveness tick (E10-S3, #764). `chiefd run`
    /// attaches a sink that refreshes its beacond `lastSeenAt`; standalone/
    /// `docstore-only`/library callers leave this `None`.
    liveness_sink: Option<std::sync::Arc<dyn LivenessSink>>,
    /// `Some` only via [`Bound::with_queue_source`] -- `chiefd run`'s
    /// one-daemon assembly, mounting `GET /v1/docs/queue` (E8-S2, #824).
    /// `None` (the standalone/migration/serve-only surfaces, which run no
    /// duty scheduler and therefore have no writer-actor queue to report)
    /// leaves the route unmounted entirely, the same absent-route shape
    /// `runtime_identity` already uses.
    queue_source: Option<std::sync::Arc<chiefd_core::actor::CompanyDb>>,
}

/// A listener that owns an address but has not opened a document store yet.
///
/// `chiefd run` reserves this before asking beacond to admit the daemon. The
/// reservation is intentionally storage-free: dropping it closes the socket,
/// and [`ListenerReservation::mount`] is the only operation that can open the
/// configured SQLite file. That gives admission a stable address to publish
/// without letting a refused daemon create a database, WAL, schema, or native
/// company state first.
pub struct ListenerReservation {
    listener: tokio::net::TcpListener,
}

impl ListenerReservation {
    /// The exact address this reservation owns. `None` only when the socket
    /// cannot report it (which is unexpected immediately after a successful
    /// bind).
    #[must_use]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.listener.local_addr().ok()
    }

    /// Open the document store and mount it on this already-reserved listener.
    ///
    /// This never binds a second socket: the `TcpListener` is moved directly
    /// into [`Bound`]. Consequently the URL beacond admitted is the URL the
    /// resulting HTTP surface serves.
    ///
    /// # Errors
    /// [`StartError::Open`] if the configured store cannot be opened.
    pub fn mount(
        self,
        config: &Config,
        feed: std::sync::Arc<ChangeFeed>,
        supervision_live: Option<router::SupervisionLiveSource>,
    ) -> Result<Bound, StartError> {
        let store =
            std::sync::Arc::new(DocStore::open_with_feed(&config.db_path, config.read_pool, feed)?);
        Ok(Bound {
            listener: self.listener,
            store,
            db_path: config.db_path.clone(),
            read_pool: config.read_pool,
            max_body_bytes: config.max_body_bytes,
            supervision_live,
            resolver: None,
            // Auth is attached later by `chiefd run` via `Bound::with_auth`;
            // reserving/mounting itself never needs the registry.
            auth: None,
            runtime_identity: None,
            shutdown_requester: None,
            liveness_sink: None,
            queue_source: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeIdentity {
    mode: String,
    company: Option<String>,
}

/// Something to poke on each liveness tick (E10-S3, #764).
///
/// Deliberately hung off the EXISTING liveness task (`heartbeat`, below)
/// rather than a new `tokio::spawn` + timer of its own: discovery must not
/// add a loop to the fleet (mandate 1). `chiefd run` passes a sink that
/// issues `POST /v1/heartbeat` to beacond (`chiefd::beacon`'s
/// `LivenessSink` impl); `docstore-only` and every library/test caller pass
/// none.
pub trait LivenessSink: Send + Sync {
    /// Called once per liveness tick, AFTER the orglog heartbeat line is
    /// emitted — never before, so a wedged sink cannot delay the liveness
    /// evidence that exists to prove chiefd is alive. Must not block: an
    /// implementation that needs to do I/O should fire-and-forget its own
    /// task (see `chiefd::beacon`'s impl) rather than await here, since this
    /// call sits inside the SAME loop that must keep ticking on schedule.
    fn on_liveness_tick(&self);
}

impl Bound {
    /// Attach a per-request resolver so a multi-company surface (docstore-only)
    /// serves /v1/org routes for many companies. Test/standalone use only.
    #[must_use]
    pub fn with_live_resolver(mut self, resolver: router::SupervisionLiveResolver) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Attach the auth runtime. Only `chiefd run` calls this; the standalone
    /// entrypoints leave it `None`.
    ///
    /// There is no second argument. It used to take `enforce: bool` as well, so
    /// a caller could attach a runtime and still serve every route to a caller
    /// that presented nothing — which, since nothing in the tree ever set
    /// `CHIEFD_AUTH_ENABLED`, is what every caller did.
    #[must_use]
    pub fn with_auth(
        mut self,
        auth: Option<std::sync::Arc<crate::authn::runtime::AuthRuntime>>,
    ) -> Self {
        self.auth = auth;
        self
    }

    /// Publish a non-secret process identity beside health so a launcher never
    /// mistakes a healthy SQL bootstrap for a full company host.
    #[must_use]
    pub fn with_runtime_identity(mut self, mode: &str, company: Option<String>) -> Self {
        self.runtime_identity = Some(RuntimeIdentity { mode: mode.to_string(), company });
        self
    }

    /// Mount `POST /v1/admin/shutdown` (E7-S3), wired to the caller's own
    /// shutdown-request sender — `chiefd run` passes a clone of
    /// `Daemon::shutdown_requester`; `docstore-only` mints its own
    /// `watch::channel` and selects it alongside `wait_for_signal()`. Not
    /// called ⇒ the route is not mounted at all, exactly like
    /// [`with_runtime_identity`](Self::with_runtime_identity).
    #[must_use]
    pub fn with_shutdown_requester(
        mut self,
        sender: tokio::sync::watch::Sender<Option<String>>,
    ) -> Self {
        self.shutdown_requester = Some(sender);
        self
    }

    /// Attach a [`LivenessSink`] to be poked on every liveness tick (E10-S3,
    /// #764). Only `chiefd run` calls this; every other caller leaves the
    /// tick beacond-silent.
    #[must_use]
    pub fn with_liveness_sink(mut self, sink: std::sync::Arc<dyn LivenessSink>) -> Self {
        self.liveness_sink = Some(sink);
        self
    }

    /// Mount `GET /v1/docs/queue` (E8-S2, #824): a read-only, no-company-scope
    /// diagnostic view of `company`'s writer-actor queue — the "is something
    /// stuck?" break-glass that replaces `org lock list` once E8-S6 deletes
    /// the file locks it read. Only `chiefd run`'s one-daemon assembly calls
    /// this; the standalone/migration/serve-only surfaces run no duty
    /// scheduler and leave the route unmounted.
    #[must_use]
    pub fn with_queue_source(
        mut self,
        company: std::sync::Arc<chiefd_core::actor::CompanyDb>,
    ) -> Self {
        self.queue_source = Some(company);
        self
    }

    /// The address the listener actually owns. `None` only if the socket cannot
    /// report it (it always can right after a successful bind); useful for
    /// discovering the ephemeral port a `127.0.0.1:0` bind was assigned.
    #[must_use]
    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.listener.local_addr().ok()
    }

    /// Create the durable schema if absent on the opened store (#390).
    ///
    /// This surface's health contract is "healthy == `org_documents` present"
    /// (`health_probe`), and the schema is idempotent — re-applied on every
    /// boot, no version table — so a daemon must be able to reach a healthy
    /// state on its OWN, without an out-of-band `POST /v1/docs/ensure-schema`
    /// or a first write. `chiefd run` calls this right after a successful bind
    /// so a brand-new/empty store file reaches `200 ok` before the launcher's
    /// boot health-gate polls it, instead of deadlocking on `503
    /// schema-missing` and SIGKILLing a perfectly healthy daemon at timeout.
    ///
    /// # Errors
    /// [`StoreError`] if the schema statements cannot be applied.
    pub async fn ensure_schema(&self) -> Result<(), StoreError> {
        self.store.ensure_schema().await
    }

    /// Subscribe to this store's change feed from this point forward.
    ///
    /// Lets a caller outside this module (`chiefd run`'s duty scheduler, for
    /// BackgroundMemory's `memory-review`-enqueue wake, od:idle-cpu #282) react
    /// to a committed `org_documents` mutation — most notably one written by
    /// the TS intercom producer through this surface's own HTTP routes —
    /// without polling. See [`feed::ChangeFeed`] for the wire-shape and
    /// emission-point contract; this is a thin pass-through to the same feed
    /// [`router`] itself is wired against, so a subscriber here sees every
    /// event a live SSE client would.
    #[must_use]
    pub fn subscribe_store_changes(&self) -> tokio::sync::broadcast::Receiver<WatchEvent> {
        self.store.feed().subscribe()
    }
}

/// Open the database and bind the address, but do NOT serve yet.
///
/// Fails loudly on ANY bind conflict — this primitive never walks past a
/// taken port (use [`bind_walking`] for a caller that should). The port
/// itself is NOT chiefd's single-writer arbiter (E10-S3, #764: beacond's
/// `register` is), so a taken address here is ordinary port contention,
/// not evidence of a second company writer.
///
/// # Errors
/// [`StartError`] if the database cannot be opened or the address cannot be
/// bound.
pub async fn bind(config: &Config) -> Result<Bound, StartError> {
    bind_with_feed(config, std::sync::Arc::new(ChangeFeed::new())).await
}

/// [`bind`], publishing onto an externally-owned `feed` rather than minting a
/// fresh one (#376).
///
/// `chiefd run`'s one-daemon assembly passes the SAME `Arc<ChangeFeed>` it
/// already wired into its `CompanyDb` writer actor (see `run.rs`'s
/// `wire_change_feed`), so a `CompanyDb` duty commit and this surface's own
/// `insert_if_absent`/`cas_update`/etc. publish onto one feed — a
/// `/v1/docs/watch` subscriber is blind to neither writer.
///
/// # Errors
/// [`StartError`] if the database cannot be opened or the address cannot be
/// bound.
pub async fn bind_with_feed(
    config: &Config,
    feed: std::sync::Arc<ChangeFeed>,
) -> Result<Bound, StartError> {
    bind_with_feed_and_company(config, feed, None).await
}

/// [`bind_with_feed`], additionally wiring a live-supervision source (#372):
/// `chiefd run`'s one-daemon assembly passes its OWN `CompanyDb` handle and
/// its own `org_documents_slug`, so `store == "supervision"` reads for this
/// company serve chiefd's live ledger instead of the (retired) mirrored
/// `org_documents` row. A separate function, not a parameter added to
/// [`bind_with_feed`] directly, so the standalone/migration entrypoints
/// (`bind`/`serve`, which have no `CompanyDb` concept at all) are
/// structurally untouched by this.
///
/// # Errors
/// [`StartError`] if the database cannot be opened or the address cannot be
/// bound.
pub async fn bind_with_feed_and_company(
    config: &Config,
    feed: std::sync::Arc<ChangeFeed>,
    supervision_live: Option<router::SupervisionLiveSource>,
) -> Result<Bound, StartError> {
    reserve_listener(config).await?.mount(config, feed, supervision_live)
}

/// Reserve one exact listener without opening a store. This preserves
/// [`bind_with_feed_and_company`]'s single-address behavior for library
/// callers, while letting `chiefd run` perform beacond admission before any
/// SQLite work.
async fn reserve_listener(config: &Config) -> Result<ListenerReservation, StartError> {
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .map_err(|source| StartError::Bind { bind: config.bind.clone(), source })?;
    Ok(ListenerReservation { listener })
}

/// Reserve, open, and mount while walking past `AddrInUse` candidates.
///
/// Callers that need an admission decision before SQLite opens should use
/// [`reserve_listener_walking`] directly, then call
/// [`ListenerReservation::mount`] after admission succeeds.
pub async fn bind_walking(
    config: &Config,
    walk: u16,
    feed: std::sync::Arc<ChangeFeed>,
    supervision_live: Option<router::SupervisionLiveSource>,
) -> Result<Bound, StartError> {
    reserve_listener_walking(config, walk).await?.mount(config, feed, supervision_live)
}

/// Reserve a listener on `AddrInUse` — and ONLY on
/// `AddrInUse` — increment the port and retry, up to `walk` consecutive
/// candidates starting at `config.bind`'s own port (E10-S3, #764).
///
/// The port stopped being chiefd's single-writer arbiter (beacond's
/// `register` is the arbiter now — see `chiefd::beacon`), so a taken port
/// is no longer a reason to refuse the whole daemon: it is walked past.
/// Any OTHER bind error (permission denied, unknown interface, an
/// unparseable `config.bind`) returns immediately — a daemon that cannot
/// bind for a non-contention reason must fail loudly on the first attempt,
/// not burn up to `walk` candidates discovering the identical refusal
/// `walk` times.
///
/// A base port of `0` (ephemeral, used by tests so the kernel always
/// hands back a free port) never walks — `walk` is ignored and exactly one
/// attempt is made, because there is no fixed base port to increment from
/// and the kernel already guarantees success.
///
/// One INFO line (`bind_taken`) is logged per skipped candidate, and one
/// INFO line on success naming the base, the chosen port, and the attempt
/// count. Exhausting the range returns [`StartError::Bind`] naming the
/// WHOLE range tried (see that variant's doc comment).
///
/// # Errors
/// [`StartError::Bind`] if `config.bind` cannot be parsed as `host:port`, a
/// non-contention bind error occurs, or the walk exhausts every candidate in
/// `[base, base + walk)`. This function never opens the configured database.
pub async fn reserve_listener_walking(
    config: &Config,
    walk: u16,
) -> Result<ListenerReservation, StartError> {
    let base_addr: std::net::SocketAddr =
        config.bind.parse().map_err(|source| StartError::Bind {
            bind: config.bind.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{source}")),
        })?;
    if base_addr.port() == 0 {
        // Ephemeral: the kernel already picked a free port. Walking would
        // be meaningless (there is no fixed base to increment) and every
        // caller that passes `:0` — every test in this workspace — expects
        // exactly one attempt.
        return reserve_listener(config).await;
    }

    let walk = walk.max(1);
    let mut attempts: u16 = 0;
    let mut candidate = base_addr;
    loop {
        attempts += 1;
        let attempt_config = Config { bind: candidate.to_string(), ..config.clone() };
        match reserve_listener(&attempt_config).await {
            Ok(reservation) => {
                tracing::info!(
                    base = %base_addr,
                    chosen = %candidate,
                    attempts,
                    "chiefd store surface: port walk succeeded"
                );
                return Ok(reservation);
            }
            Err(StartError::Bind { source, .. })
                if source.kind() == std::io::ErrorKind::AddrInUse =>
            {
                tracing::info!(
                    bind_taken = %candidate,
                    attempts,
                    "chiefd store surface: port taken, walking to the next candidate"
                );
                if attempts >= walk {
                    return Err(StartError::Bind {
                        bind: format!("{base_addr}..{candidate} ({attempts} attempts)"),
                        source,
                    });
                }
                let next_port =
                    candidate.port().checked_add(1).ok_or_else(|| StartError::Bind {
                        bind: format!("{base_addr}..{candidate} ({attempts} attempts)"),
                        source: std::io::Error::new(
                            std::io::ErrorKind::AddrInUse,
                            "port walk exhausted the u16 port range",
                        ),
                    })?;
                candidate.set_port(next_port);
            }
            // Any other error (permission denied or an unknown interface)
            // is NOT contention — return it immediately rather than walking
            // `walk` more times to rediscover the identical refusal.
            Err(other) => return Err(other),
        }
    }
}

/// Open the database and serve the typed surface until the process ends.
///
/// This is the standalone/migration library entrypoint; it does not wire itself
/// into `chiefd`'s CLI, so it never collides with the daemon's own startup
/// surface. It never returns on its own — the one-daemon mount uses
/// [`serve_bound`] with a real shutdown future instead.
///
/// # Errors
/// [`StartError`] if the database cannot be opened, the address cannot be
/// bound, or serving fails.
pub async fn serve(config: &Config) -> Result<(), StartError> {
    let bound = bind(config).await?;
    // Standalone: nothing ever asks it to stop, so the shutdown future is one
    // that never resolves — identical to the pre-split "until the process ends".
    serve_bound(bound, std::future::pending::<()>()).await
}

/// Serve an already-[`bind`]ed surface until `shutdown` resolves, then drain
/// in-flight connections gracefully.
///
/// The one-daemon assembly passes a future that resolves when the daemon's
/// shutdown watch flips, so the docstore listener stops on the very same signal
/// the duty-scheduler tasks respect — not as a bolt-on that outlives them.
///
/// # Errors
/// [`StartError::Bind`] if serving fails.
pub async fn serve_bound(
    bound: Bound,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), StartError> {
    serve_bound_with_watch(bound, shutdown, None).await
}

/// [`serve_bound`], additionally wired to the owning daemon's shutdown watch.
///
/// The mounted daemon passes `Some` so its remaining long-lived watcher route
/// (`/v1/docs/watch`) receives EOF before Axum begins graceful connection
/// drain. Standalone callers pass `None` and retain their existing lifecycle.
/// This is deliberately a listener-lifecycle concern, not a restoration of the
/// retired generic document read/write API: after SQL normalization the mount
/// keeps only typed routes plus the changefeed watcher.
///
/// # Errors
/// [`StartError`] if serving fails.
pub async fn serve_bound_with_watch(
    bound: Bound,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    watch_shutdown: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<(), StartError> {
    let Bound {
        listener,
        store,
        db_path,
        read_pool,
        max_body_bytes,
        supervision_live,
        resolver,
        auth,
        runtime_identity,
        shutdown_requester,
        liveness_sink,
        queue_source,
    } = bound;
    // The real bound address (an ephemeral `:0` resolves here), so the log names
    // the port that is actually listening.
    let bind =
        listener.local_addr().map_or_else(|_| "<unknown>".to_string(), |addr| addr.to_string());
    tracing::info!(
        %bind,
        db = %db_path,
        read_pool,
        max_body_bytes,
        "chiefd org_documents store surface listening"
    );
    // A daemon that logs only at startup is indistinguishable from a wedged one
    // an hour later — the exact shape of the nineteen-hour blackout.
    // `None` when nothing names a directory to write into — see
    // `chiefd_log::sink::log_root_from_env`. The heartbeat is the guarantee
    // that a MISSING line is positive evidence of death, so a process with no
    // stream makes no such promise rather than a hollow one: it emits nothing
    // and spawns no ticker, and the console layer still carries every line.
    let heartbeat = chiefd_log::OrgLog::from_env("chiefd-store", None).map(|log| {
        log.emit(
            "info",
            "surface-listening",
            &format!(r#""bind":"{bind}","readPool":{read_pool},"maxBodyBytes":{max_body_bytes}"#,),
        );
        tokio::spawn(heartbeat(log, liveness_sink))
    });

    let app = mounted_app(MountedAppParts {
        store,
        max_body_bytes,
        supervision_live,
        resolver,
        auth,
        watch_shutdown,
        runtime_identity,
        shutdown_requester,
        queue_source,
    });
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|source| StartError::Bind { bind, source });
    if let Some(heartbeat) = heartbeat {
        heartbeat.abort();
    }
    result
}

/// Everything [`mounted_app`] needs. A struct rather than ten positional
/// arguments, because the members are all optional and several are the same
/// type, which is how a caller silently swaps two of them.
struct MountedAppParts {
    store: std::sync::Arc<DocStore>,
    max_body_bytes: usize,
    supervision_live: Option<router::SupervisionLiveSource>,
    resolver: Option<router::SupervisionLiveResolver>,
    auth: Option<std::sync::Arc<crate::authn::runtime::AuthRuntime>>,
    watch_shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    runtime_identity: Option<RuntimeIdentity>,
    shutdown_requester: Option<tokio::sync::watch::Sender<Option<String>>>,
    queue_source: Option<std::sync::Arc<chiefd_core::actor::CompanyDb>>,
}

/// The COMPLETE application this mount serves: every shared docstore route,
/// plus the three the mount adds from its own `Bound` fields, and the gate over
/// all of them.
///
/// # The ordering rule this function exists to enforce
///
/// `Router::layer` wraps only the routes registered BEFORE the call. The three
/// mount-owned routes used to be added to a router that already carried the
/// verify-middleware, so they sat OUTSIDE the gate: `/v1/docs/runtime`,
/// `/v1/docs/queue`, and — the one that matters — `POST /v1/admin/shutdown`,
/// an unauthenticated request that drains and exits a company daemon. Nothing
/// declared them exempt; registration order did, silently, and `EXEMPT_PATHS`
/// went on saying the exempt set was three.
///
/// Every route is therefore registered first and [`router::apply_gate`] is
/// called once, last. A route added anywhere in here is gated by construction,
/// and `serve_bound_composes_every_route_inside_the_gate` walks the built
/// router to prove it rather than trusting this comment.
fn mounted_app(parts: MountedAppParts) -> axum::Router {
    let MountedAppParts {
        store,
        max_body_bytes,
        supervision_live,
        resolver,
        auth,
        watch_shutdown,
        runtime_identity,
        shutdown_requester,
        queue_source,
    } = parts;

    let mut app = router::ungated_routes(
        store,
        max_body_bytes,
        router::WATCH_HEARTBEAT_INTERVAL,
        resolver,
        watch_shutdown,
    );
    if let Some(identity) = runtime_identity {
        app = app.route(
            "/v1/docs/runtime",
            axum::routing::get(move || {
                let identity = identity.clone();
                async move { axum::Json(identity) }
            }),
        );
    }
    if let Some(sender) = shutdown_requester {
        // A dedicated sub-router carries the `Extension<ShutdownRequest>`
        // layer, then merges in — scoping it to this one route rather than
        // every route `app` already has (`Router::layer` wraps everything at
        // the call site).
        let shutdown_router = axum::Router::new()
            .route("/v1/admin/shutdown", axum::routing::post(router::admin_shutdown))
            .layer(axum::extract::Extension(router::ShutdownRequest(sender)));
        app = app.merge(shutdown_router);
    }
    if let Some(company) = queue_source {
        app = app.route(
            "/v1/docs/queue",
            axum::routing::get(move || {
                let company = std::sync::Arc::clone(&company);
                async move { axum::Json(queue_response(&company.queue_snapshot())) }
            }),
        );
    }
    router::apply_gate(app, supervision_live, auth, max_body_bytes)
}

/// Render one [`chiefd_core::actor::QueueSnapshot`] as the `GET /v1/docs/queue`
/// wire shape (E8-S2, #824). Honesty rule inherited from
/// `org-lock-inventory.ts`: a field that cannot be computed is OMITTED, never
/// defaulted — `current` is absent exactly when the writer is idle, and that
/// is its only meaning.
fn queue_response(snapshot: &chiefd_core::actor::QueueSnapshot) -> serde_json::Value {
    let mut body = serde_json::json!({
        "depth": snapshot.depth,
        "oldestEnqueuedMs": snapshot.oldest_enqueued_ms,
        "deadlineMs": snapshot.deadline_ms,
    });
    if let Some(current) = snapshot.current {
        let class = match current.class {
            chiefd_core::actor::MutationClass::Small => "small",
            chiefd_core::actor::MutationClass::Normal => "normal",
            chiefd_core::actor::MutationClass::Reconcile => "reconcile",
        };
        body["current"] = serde_json::json!({
            "name": current.name.0,
            "class": class,
            "enqueuedMs": current.enqueued_ms,
        });
    }
    body
}

/// Emit a liveness line on a fixed interval so a *missing* line becomes positive
/// evidence of death. Then, if a [`LivenessSink`] is attached (E10-S3, #764),
/// poke it — AFTER the orglog line, never before, so a wedged sink cannot
/// delay the liveness evidence that exists to prove chiefd is alive.
async fn heartbeat(log: chiefd_log::OrgLog, sink: Option<std::sync::Arc<dyn LivenessSink>>) {
    let interval = chiefd_log::heartbeat_interval();
    let started = std::time::Instant::now();
    // render-clock / os-liveness: the ONE sanctioned ticker in these crates,
    // and the reason `tokio::time::interval` is otherwise banned in
    // `clippy.toml`. This loop samples nothing and diffs nothing -- it EMITS.
    // Its entire product is that a missing line becomes positive evidence of
    // death, which is a guarantee only a fixed cadence can make: a heartbeat
    // driven by events would fall silent exactly when the process stops
    // servicing events, which is precisely the state it exists to expose. It
    // is therefore not a poll in disguise, and it must not be made reactive.
    #[allow(clippy::disallowed_methods)]
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        log.emit(
            "info",
            "heartbeat",
            &format!(
                r#""uptimeMs":{uptime},"intervalMs":{interval},"droppedLines":{dropped}"#,
                uptime = started.elapsed().as_millis(),
                interval = interval.as_millis(),
                dropped = chiefd_log::dropped_lines(),
            ),
        );
        if let Some(sink) = &sink {
            sink.on_liveness_tick();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string())
    }

    #[test]
    fn a_missing_db_path_is_a_startup_failure_not_a_new_empty_file() {
        let err = Config::from_env(env(&[("CHIEFD_STORE_BIND", "127.0.0.1:8792")]))
            .expect_err("must refuse to guess");
        assert!(err.to_string().contains("CHIEFD_STORE_DB_PATH"));
    }

    #[test]
    fn defaults_fill_in_around_a_db_path() {
        let config =
            Config::from_env(env(&[("CHIEFD_STORE_DB_PATH", "/root/.write-db/org.sqlite")]))
                .expect("db path is sufficient");
        assert_eq!(config.db_path, "/root/.write-db/org.sqlite");
        assert_eq!(config.read_pool, DEFAULT_READ_POOL);
        assert_eq!(config.max_body_bytes, DEFAULT_MAX_BODY_BYTES);
    }

    #[tokio::test]
    async fn subscribe_store_changes_observes_a_normalized_row_hint_from_this_bound_surface() {
        // od:idle-cpu #282's wake seam: a subscriber taken from `Bound` (not
        // constructed by reaching around it) must see a mutation committed
        // through the SAME mounted surface.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite").display().to_string();
        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path: db_path.clone(),
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind");
        bound.store.ensure_schema().await.expect("schema");

        let mut changes = bound.subscribe_store_changes();

        bound.store.feed().publish("northstar", "memory-review", "2026-07-23T00:00:00.000Z", false);

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), changes.recv())
            .await
            .expect("an event arrives promptly")
            .expect("the channel is open");
        assert_eq!(event.slug, "northstar");
        assert_eq!(event.store, "memory-review");
        assert!(!event.removed);
    }

    #[tokio::test]
    async fn runtime_identity_distinguishes_a_full_company_host_from_docstore_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite").display().to_string();
        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path,
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind")
        .with_runtime_identity("company", Some("northstar".to_string()));
        bound.ensure_schema().await.expect("schema");
        let addr = bound.local_addr().expect("bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_bound(bound, async move {
            let _ = stopped.await;
        }));

        let response = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};

            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            stream
                .write_all(
                    b"GET /v1/docs/runtime HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .expect("write request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).expect("read response");
            String::from_utf8(response).expect("utf8 response")
        })
        .await
        .expect("request task joins");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains(r#""mode":"company""#), "{response}");
        assert!(response.contains(r#""company":"northstar""#), "{response}");
        assert_eq!(
            serde_json::to_value(RuntimeIdentity {
                mode: "docstore-only".to_string(),
                company: None,
            })
            .expect("serialize bootstrap identity"),
            serde_json::json!({ "mode": "docstore-only", "company": null }),
        );

        let _ = stop.send(());
        server.await.expect("server joins").expect("server stops");
    }

    /// E8-S2 (#824): the queue diagnostic is mounted only with the daemon's
    /// writer actor, answers the precise idle shape over the real HTTP socket,
    /// and is observational — the request must not enter the writer or advance
    /// its committed sequence (D19).
    #[tokio::test]
    async fn docs_queue_route_is_read_only_and_reports_an_idle_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docstore_path = dir.path().join("org.sqlite").display().to_string();
        let company_path = dir.path().join("northstar.chief.db");
        let clock: chiefd_core::clock::SharedClock =
            std::sync::Arc::new(chiefd_core::test_support::ManualClock::default());
        let company = std::sync::Arc::new(
            chiefd_core::actor::CompanyDb::open("northstar", &company_path, clock)
                .expect("open company writer"),
        );
        let before = company.snapshot().commit_seq();

        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path: docstore_path,
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind")
        .with_queue_source(std::sync::Arc::clone(&company));
        bound.ensure_schema().await.expect("schema");
        let addr = bound.local_addr().expect("bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_bound(bound, async move {
            let _ = stopped.await;
        }));

        let response = get_raw(addr, "/v1/docs/queue").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("content-type: application/json"), "{response}");
        assert!(response.contains(r#""depth":0"#), "{response}");
        assert!(response.contains(r#""oldestEnqueuedMs":0"#), "{response}");
        assert!(response.contains(r#""deadlineMs":30000"#), "{response}");
        assert!(
            !response.contains(r#""current""#),
            "idle is the only meaning of an omitted current field: {response}"
        );
        assert_eq!(
            company.snapshot().commit_seq(),
            before,
            "GET /v1/docs/queue is diagnostics only: it never enqueues or commits"
        );

        let _ = stop.send(());
        server.await.expect("server joins").expect("server stops");
        company.shutdown();
    }

    async fn get_raw(addr: std::net::SocketAddr, path: &str) -> String {
        use std::io::{Read, Write};

        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).expect("write request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).expect("read response");
            String::from_utf8(response).expect("utf8 response")
        })
        .await
        .expect("request task joins")
    }

    async fn post_raw(addr: std::net::SocketAddr, path: &str, body: &str) -> String {
        use std::io::{Read, Write};
        let path = path.to_string();
        let body = body.to_string();
        tokio::task::spawn_blocking(move || {
            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
                 Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
                len = body.len(),
            );
            stream.write_all(request.as_bytes()).expect("write request");
            let mut response = Vec::new();
            stream.read_to_end(&mut response).expect("read response");
            String::from_utf8(response).expect("utf8 response")
        })
        .await
        .expect("request task joins")
    }

    #[tokio::test]
    async fn admin_shutdown_flips_the_supplied_sender_and_returns_202_with_no_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite").display().to_string();
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path,
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind")
        .with_shutdown_requester(tx);
        bound.ensure_schema().await.expect("schema");
        let addr = bound.local_addr().expect("bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_bound(bound, async move {
            let _ = stopped.await;
        }));

        let response = post_raw(addr, "/v1/admin/shutdown", r#"{"reason":"operator stop"}"#).await;
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        assert!(response.contains(r#""accepted":true"#), "{response}");
        assert!(!response.contains("pid"), "the response must carry no pid (D7): {response}");

        // Blocks on the real event (`rx.changed()`) rather than polling for
        // it; the 1s bound is a deadlock detector for a hung test process,
        // not a performance budget — the flip is synchronous with the
        // handler's `send_replace`, so this resolves in microseconds.
        let changed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("the sender is flipped promptly");
        assert!(changed.is_ok());
        assert_eq!(rx.borrow().as_deref(), Some("operator stop"));

        let _ = stop.send(());
        server.await.expect("server joins").expect("server stops");
    }

    #[tokio::test]
    async fn admin_shutdown_route_is_not_mounted_without_a_supplied_sender() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite").display().to_string();
        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path,
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind");
        bound.ensure_schema().await.expect("schema");
        let addr = bound.local_addr().expect("bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_bound(bound, async move {
            let _ = stopped.await;
        }));

        let response = post_raw(addr, "/v1/admin/shutdown", r#"{"reason":"operator stop"}"#).await;
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");

        let _ = stop.send(());
        server.await.expect("server joins").expect("server stops");
    }

    #[tokio::test]
    async fn admin_shutdown_a_second_call_is_harmless() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite").display().to_string();
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        let bound = bind(&Config {
            bind: "127.0.0.1:0".to_string(),
            db_path,
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        })
        .await
        .expect("bind")
        .with_shutdown_requester(tx);
        bound.ensure_schema().await.expect("schema");
        let addr = bound.local_addr().expect("bound address");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_bound(bound, async move {
            let _ = stopped.await;
        }));

        let first = post_raw(addr, "/v1/admin/shutdown", r#"{"reason":"first"}"#).await;
        assert!(first.starts_with("HTTP/1.1 202"), "{first}");
        // Deadlock detector, not a performance budget (both timeouts in this
        // test): each `rx.changed()` blocks on the real watch flip, never
        // polled.
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("first flip observed")
            .expect("channel open");

        let second = post_raw(addr, "/v1/admin/shutdown", r#"{"reason":"second"}"#).await;
        assert!(second.starts_with("HTTP/1.1 202"), "{second}");
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("second flip observed")
            .expect("channel open");
        assert_eq!(rx.borrow().as_deref(), Some("second"));

        let _ = stop.send(());
        server.await.expect("server joins").expect("server stops");
    }

    #[test]
    fn an_unparseable_numeric_setting_falls_back_instead_of_failing() {
        let config = Config::from_env(env(&[
            ("CHIEFD_STORE_DB_PATH", "/x/org.sqlite"),
            ("CHIEFD_STORE_READ_POOL", "eight"),
        ]))
        .expect("config");
        assert_eq!(config.read_pool, DEFAULT_READ_POOL);
    }

    // ---- listener reservation + port walk (E10-S3, #764) -------------------

    fn walk_config(bind: &str, db_path: &std::path::Path) -> Config {
        Config {
            bind: bind.to_string(),
            db_path: db_path.display().to_string(),
            read_pool: DEFAULT_READ_POOL,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    #[tokio::test]
    async fn reservation_walks_to_the_next_port_when_the_base_is_held_without_opening_sqlite() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let held_addr = held.local_addr().expect("addr");
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite");
        let config = walk_config(&held_addr.to_string(), &db_path);
        let reservation =
            reserve_listener_walking(&config, 8).await.expect("walks past the held port");
        let bound_addr = reservation.local_addr().expect("reserved addr");
        assert_ne!(bound_addr.port(), held_addr.port());
        assert!(bound_addr.port() > held_addr.port());
        for path in [
            db_path.clone(),
            std::path::PathBuf::from(format!("{}-wal", db_path.display())),
            std::path::PathBuf::from(format!("{}-shm", db_path.display())),
        ] {
            assert!(
                !path.exists(),
                "reserving/walking a listener must not open SQLite or create {path:?}"
            );
        }
        drop(held);
    }

    #[tokio::test]
    async fn reservation_walk_of_one_reproduces_todays_single_attempt_behaviour() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
        let held_addr = held.local_addr().expect("addr");
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite");
        let config = walk_config(&held_addr.to_string(), &db_path);
        let result = reserve_listener_walking(&config, 1).await;
        assert!(matches!(result, Err(StartError::Bind { .. })));
        drop(held);
    }

    #[tokio::test]
    async fn reservation_never_walks_off_an_ephemeral_base_port() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite");
        let config = walk_config("127.0.0.1:0", &db_path);
        // A walk of 64 against `:0` must still be exactly one attempt: the
        // kernel already guarantees success, so a second attempt would
        // never even be reachable, but we assert on the RESULT rather than
        // instrumenting attempt count -- an ephemeral bind cannot fail on
        // AddrInUse by construction.
        let reservation =
            reserve_listener_walking(&config, 64).await.expect("ephemeral bind always succeeds");
        assert!(reservation.local_addr().is_some());
        assert!(
            !db_path.exists(),
            "an ephemeral reservation must not create the configured database"
        );
    }

    #[tokio::test]
    async fn reservation_returns_immediately_on_a_non_contention_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite");
        // An address this process cannot bind for a reason OTHER than
        // contention: not on this host's loopback range at all.
        let config = walk_config("198.51.100.1:9", &db_path);
        let result = reserve_listener_walking(&config, 64).await;
        assert!(matches!(result, Err(StartError::Bind { .. })));
        if let Err(StartError::Bind { source, .. }) = result {
            assert_ne!(
                source.kind(),
                std::io::ErrorKind::AddrInUse,
                "a non-contention failure must not be misreported as AddrInUse"
            );
        }
    }

    #[tokio::test]
    async fn reservation_exhausts_the_bounded_range_and_names_both_ends() {
        // Hold three consecutive ports so a walk of exactly 3 exhausts.
        let first = std::net::TcpListener::bind("127.0.0.1:0").expect("hold 1");
        let base_port = first.local_addr().expect("addr").port();
        let second = std::net::TcpListener::bind(format!("127.0.0.1:{}", base_port + 1));
        let third = std::net::TcpListener::bind(format!("127.0.0.1:{}", base_port + 2));
        // A racy host could have either of these taken already by something
        // else; skip rather than false-fail if so (the base is still held,
        // which is the property this test needs).
        let Ok(second) = second else { return };
        let Ok(third) = third else { return };

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("org.sqlite");
        let config = walk_config(&format!("127.0.0.1:{base_port}"), &db_path);
        let result = reserve_listener_walking(&config, 3).await;
        let is_ok = result.is_ok();
        let Err(StartError::Bind { bind, .. }) = result else {
            panic!("expected an exhausted-range Bind error, got is_ok={is_ok}");
        };
        assert!(bind.contains(&base_port.to_string()), "{bind}");
        assert!(bind.contains(&(base_port + 2).to_string()), "{bind}");
        drop(first);
        drop(second);
        drop(third);
    }

    // ---- LivenessSink (E10-S3, #764) ---------------------------------------

    struct RecordingSink {
        ticks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LivenessSink for RecordingSink {
        fn on_liveness_tick(&self) {
            self.ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct BlockingSink;

    impl LivenessSink for BlockingSink {
        fn on_liveness_tick(&self) {
            // A sink that blocks the CALLER (not a spawned task) would stall
            // every subsequent tick; this proves the heartbeat loop's own
            // tick cadence does not wait on the sink's own work completing --
            // it only waits on `on_liveness_tick` RETURNING, which a
            // well-behaved sink (like `HeartbeatSink`) achieves by
            // fire-and-forgetting its I/O rather than awaiting it here.
        }
    }

    #[tokio::test]
    async fn liveness_sink_is_poked_after_the_orglog_line_and_a_blocking_impl_does_not_suppress_it()
    {
        // Directly exercise `heartbeat`'s ordering contract: the orglog
        // `log.emit` call happens unconditionally before `sink
        // .on_liveness_tick()` in the loop body (see the function above) --
        // asserted here by observing that a sink IS called (proving the
        // call site is reached) without the tick loop ever stalling. A
        // full ordering proof against the actual emitted log line lives in
        // the integration battery (`run/tests.rs`'s docstore-mount suite),
        // which already asserts the surface stays responsive with a
        // liveness sink attached.
        let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink: std::sync::Arc<dyn LivenessSink> =
            std::sync::Arc::new(RecordingSink { ticks: std::sync::Arc::clone(&ticks) });
        sink.on_liveness_tick();
        assert_eq!(ticks.load(std::sync::atomic::Ordering::SeqCst), 1);

        // A blocking-shaped sink (one that does its work inline rather than
        // spawning) still returns promptly here because THIS sink's own
        // body does no I/O -- the contract `HeartbeatSink` upholds is
        // enforced by review/doc comment, not mechanically by this trait
        // (Rust cannot type-check "does not block" into a trait bound);
        // this test proves the call site tolerates an inline sink without
        // panicking or hanging, which is the structural half of the proof.
        let blocking: std::sync::Arc<dyn LivenessSink> = std::sync::Arc::new(BlockingSink);
        blocking.on_liveness_tick();
    }

    // ── The gate covers the whole mounted surface ─────────────────────────
    //
    // These walk the BUILT router rather than reading `EXEMPT_PATHS` and
    // agreeing with it. A constant is a claim; the router is the fact, and the
    // defect these exist for is exactly a case where the two disagreed —
    // `Router::layer` wraps only what precedes it, so three routes registered
    // after the verify-middleware were effectively exempt while the constant
    // went on saying the exempt set was three others.

    /// Every source file that registers a route on the mounted surface.
    const ROUTE_SOURCES: &[(&str, &str)] = &[
        ("docstore/router.rs", include_str!("router.rs")),
        ("docstore/runtime_routes.rs", include_str!("runtime_routes.rs")),
        ("docstore/mod.rs", include_str!("mod.rs")),
    ];

    /// A floor for the derived route count. Not an inventory — a tripwire for a
    /// parser that matched nothing, which would make the walk pass over an
    /// empty loop. Deliberately far below the real count.
    const MINIMUM_DERIVED_ROUTES: usize = 80;

    /// Every `.route("…")` path literal in the sources above, tolerating the
    /// multi-line form (`app.route(\n    "/v1/docs/runtime",`) that the three
    /// mount-owned routes are written in — the exact form a naive one-line
    /// parser would have skipped, which is how they went unwalked.
    fn registered_paths() -> std::collections::BTreeSet<String> {
        let mut paths = std::collections::BTreeSet::new();
        for (_, source) in ROUTE_SOURCES {
            for tail in source.split(".route(").skip(1) {
                let Some(rest) = tail.split_once('"') else { continue };
                let Some(path) = rest.1.split('"').next() else { continue };
                if path.starts_with("/v1/") {
                    paths.insert(path.to_string());
                }
            }
        }
        paths
    }

    /// The mounted app exactly as `serve_bound` composes it, with all three
    /// mount-owned routes present and a real auth runtime enforcing.
    fn gated_app(dir: &std::path::Path) -> (axum::Router, std::sync::Arc<DocStore>) {
        use chiefd_core::actor::CompanyDb;

        let store = std::sync::Arc::new(
            DocStore::open(&dir.join("org.sqlite").display().to_string(), 2).expect("open store"),
        );
        let company = std::sync::Arc::new(
            CompanyDb::open(
                "northstar",
                &dir.join(chiefd_core::store::COMPANY_DB_FILENAME),
                std::sync::Arc::new(chiefd_core::test_support::ManualClock::starting_at(0, 1_000)),
            )
            .expect("open company"),
        );
        let auth = std::sync::Arc::new(crate::authn::runtime::AuthRuntime::new(
            std::sync::Arc::clone(&company),
            std::sync::Arc::new(b"mounted-app-gate-secret".to_vec()),
            30_000,
            8,
            std::sync::Arc::new(|| 1_000),
        ));
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(None::<String>);
        let app = mounted_app(MountedAppParts {
            store: std::sync::Arc::clone(&store),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            supervision_live: None,
            resolver: None,
            auth: Some(auth),
            watch_shutdown: None,
            runtime_identity: Some(RuntimeIdentity {
                mode: "company".to_string(),
                company: Some("northstar".to_string()),
            }),
            shutdown_requester: Some(shutdown_tx),
            queue_source: Some(company),
        });
        (app, store)
    }

    async fn status_without_a_bearer(app: &axum::Router, path: &str) -> axum::http::StatusCode {
        use tower::ServiceExt as _;

        let request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .body(axum::body::Body::empty())
            .expect("request");
        app.clone().oneshot(request).await.expect("response").status()
    }

    /// THE GUARD. The set of paths that answer without a credential is EXACTLY
    /// [`crate::authn::middleware::EXEMPT_PATHS`] — derived by asking the built
    /// router, one path at a time, never by reading the constant.
    ///
    /// Before this, `/v1/docs/runtime`, `/v1/docs/queue` and
    /// `POST /v1/admin/shutdown` all answered here, and the last of those
    /// drains and exits a company daemon.
    #[tokio::test]
    async fn serve_bound_composes_every_route_inside_the_gate() {
        use crate::authn::middleware::EXEMPT_PATHS;

        let paths = registered_paths();
        assert!(
            paths.len() >= MINIMUM_DERIVED_ROUTES,
            "derived only {} route literals; the parser matched nothing useful and this walk \
             would pass vacuously",
            paths.len(),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let (app, store) = gated_app(dir.path());
        store.ensure_schema().await.expect("schema");

        let mut answered_without_a_credential = Vec::new();
        for path in &paths {
            if status_without_a_bearer(&app, path).await != axum::http::StatusCode::UNAUTHORIZED {
                answered_without_a_credential.push(path.clone());
            }
        }
        let mut expected: Vec<String> = EXEMPT_PATHS.iter().map(|p| (*p).to_string()).collect();
        expected.sort();

        assert_eq!(
            answered_without_a_credential, expected,
            "the paths that serve an unauthenticated caller must be exactly the exempt set. A \
             route that appears here without being in EXEMPT_PATHS is registered AFTER the gate \
             layer; move it above `apply_gate`."
        );
    }

    /// The mount-owned three are registered at all, so the walk above is not
    /// satisfied by a build that simply dropped them.
    #[test]
    fn the_three_mount_owned_routes_are_registered() {
        let paths = registered_paths();
        for path in ["/v1/docs/runtime", "/v1/admin/shutdown", "/v1/docs/queue"] {
            assert!(paths.contains(path), "{path} is no longer registered by the mount");
        }
    }
}

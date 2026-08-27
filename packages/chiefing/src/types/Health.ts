// Types for DocsClient.probe() — the runtime-identity probe.
//
// Source: apps/chiefd/crates/chiefd-api/src/docstore/mod.rs's `RuntimeIdentity`
// (the `GET /v1/docs/runtime` response) and router.rs's `health` handler
// (`GET /v1/docs/health`, `{"status": "ok"}` on 200).

/** Rust authority: docstore/mod.rs `RuntimeIdentity`
 * (`#[serde(rename_all = "camelCase")]`). Absent entirely (the route is not
 * mounted) for a generic library caller that never called
 * `Bound::with_runtime_identity`. */
export interface DocsRuntime {
  mode: 'company' | 'docstore-only'
  company?: string
}

/** Composed client-side from `GET /v1/docs/health` + `GET /v1/docs/runtime` —
 * not itself one Rust struct. Preserves WHAT the health endpoint said so a
 * start failure can name the real reason: a 503 `{"status": "schema-missing:
 * …"}` is a daemon that is alive and answering, just not ready — not a dead
 * or wedged one. */
export interface HealthProbe {
  /** True iff HTTP 200 with body `{"status":"ok"}` — the only "ready" state. */
  ok: boolean
  /** The HTTP status the health endpoint returned. Undefined iff nothing
   * answered (connection refused/timed out). */
  httpStatus?: number
  /** The body's `status` string, or on transport failure the error text. */
  reason?: string
  /** `DocsRuntime.mode`, when the runtime-identity route is mounted. */
  runtimeMode?: 'company' | 'docstore-only'
  /** `DocsRuntime.company`, for a full company host. */
  company?: string
}

/** The mutation the single writer is executing right now. Rust authority:
 * docstore/mod.rs `queue_response`. */
export interface WriterQueueCurrent {
  name: string
  class: 'small' | 'normal' | 'reconcile'
  enqueuedMs: number
}

/** `GET /v1/docs/queue` — the "is the writer stuck or backed up?" diagnostic.
 *
 * Honesty rule, inherited from the retired lock inventory and preserved by the
 * Rust renderer: a field that cannot be computed is OMITTED, never defaulted.
 * `current` is absent exactly when the writer is idle, and absence is its only
 * meaning — a caller must not read a missing `current` as anything else.
 *
 * Contention lives here now. It is queue depth, not a held lock: there is no
 * lock inventory to inspect because there are no locks. */
export interface WriterQueueSnapshot {
  depth: number
  oldestEnqueuedMs: number | null
  deadlineMs: number | null
  current?: WriterQueueCurrent
}

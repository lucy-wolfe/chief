// Connect-refusal-only retry ladder and the optional unreachable-circuit
// breaker. Timeouts are NEVER retried — a retried timeout can double-apply a
// write. Every delay here is `await`ed (Mandate 1): no interval, no worker,
// no blocking wait primitive of any kind.

/** Awaited backoff ladder for connect refusals only (FetchTransport). */
export const CONNECT_RETRY_BACKOFFS_MS: readonly number[] = [25, 75, 150]

/** Awaited backoff ladder for the ensure-schema restart-blip class
 * (org-durable-store.ts:707). Not consumed by FetchTransport itself — a
 * later resource story retries schema-not-ready responses with this ladder. */
export const ENSURE_SCHEMA_RETRY_DELAYS_MS: readonly number[] = [100, 250, 500, 1000, 2000, 4000]

// #751/G13: `LOCK_RETRY_BASE_DELAYS_MS` is deleted. It was a ladder for
// "durable-lock busy retries (a later resource story)"; that story never
// arrived and cannot — E8-S6/S6b/S6c deleted the whole lock surface on both
// sides (`/v1/locks/*`, `org_locks`, LocksClient), so there is no busy
// refusal left for it to pace. Mandate 4 forbids the thing it was waiting on.

/**
 * Full jitter on top of a ladder rung: `[baseMs, baseMs * 2)`. `random`
 * defaults to `Math.random` and is injectable so a test can pin the bounds
 * without flaking.
 */
export function retryDelayWithJitter(baseMs: number, random: () => number = Math.random): number {
  return baseMs + random() * baseMs
}

/** `await`ed delay — the only sanctioned way this package waits on a clock. */
export function awaitedDelay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms)
  })
}

// #751/G13: `UnreachableCircuit` is deleted. It shipped "off by default —
// nothing in this story wires it into FetchTransport; a consumer (apps/api)
// opts in per client", and no consumer ever did: its only references were its
// own unit test and the public-surface assertion. FetchTransport's
// CONNECT_RETRY_BACKOFFS_MS ladder is the whole of this package's live
// connect-failure policy. (`shim/transport.ts` has an unrelated class of the
// same name; that directory is retired separately under #751/G8.)

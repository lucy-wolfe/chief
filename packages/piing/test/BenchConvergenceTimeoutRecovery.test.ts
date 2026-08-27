/**
 * A committed bench must never be reported to the manager as an outage.
 *
 * `POST /v1/org/person/bench-lifecycle` commits the bench, then holds its
 * answer until chiefd's own convergence confirms the tagged pane stopped. On
 * expiry it answers `503 bench-convergence-timeout`, whose detail begins "bench
 * committed" — a success told honestly, not a failure. `org_bench` translates
 * it back by VERIFYING the artifact: it re-reads the roster and, only if the
 * person really is benched, reports the bench as the success it is. Otherwise a
 * manager is told a bench failed that in fact succeeded, and the retry that
 * invites answers `already-benched` (the #141 inverse, in the bench family).
 *
 * That branch had no test, and for its whole life it could not fire: the route
 * waited 30s while `FetchTransport` aborts at `DEFAULT_TIMEOUT_MS` (10s), so
 * the client always gave up first. A client-side abort carries
 * `kind: 'timeout'` and NO `status`, so `error.status === 503` never matched —
 * the #1004 shape, a recovery branch that is dead code because the failure
 * never arrives in the form it expects. The route's wait is now 6s, and
 * `scripts/test/client-observable-wait.test.mjs` keeps the two numbers ordered.
 *
 * These tests pin the branch itself: it fires on a DELIVERED 503 with the
 * artifact verified, and on nothing else.
 */
import { isNullish } from '@test/support/Nullish'
import { captureRegisteredTools } from '@test/support/ToolRegistrationHarness'
import type {
  CapturedTool,
  ToolRegistrationCapture,
  ToolRegistrationOptions
} from '@test/types/ToolRegistrationHarness'
import { afterEach, describe, expect, test } from 'vitest'

const BENCH_ROUTE = '/v1/org/person/bench-lifecycle'

/** The exact body `RouteError::unavailable` puts on the wire for this route,
 *  written out rather than built: a fixture that reproduces chiefd's wire shape
 *  is only evidence if it is the literal bytes chiefd sends. */
const CONVERGENCE_TIMEOUT_BODY =
  '{"code":"bench-convergence-timeout",' +
  '"detail":"bench committed but Rust convergence did not confirm the tagged pane stopped"}'

/** An unclassified 5xx: no code the client can branch on. */
const UNCLASSIFIED_BODY = '{"detail":"unclassified"}'

function worker(employmentState: 'active' | 'benched'): Record<string, unknown> {
  return {
    bo: {
      id: 'bo',
      name: 'Bo',
      title: 'Engineer',
      kind: 'worker',
      departmentId: 'executive',
      employmentState,
      createdAt: '2026-01-01T00:00:00.000Z'
    }
  }
}

/** `toolResult`'s shape: Pi's content array plus the tool's own details, whose
 *  `ok` is the success flag the manager's surface reads. */
interface ToolResult {
  readonly content?: readonly { readonly type: string; readonly text: string }[]
  readonly details?: Record<string, unknown>
}

function text(result: ToolResult): string {
  return (result.content ?? []).map((part) => part.text).join('\n')
}

let capture: ToolRegistrationCapture | undefined

afterEach(async () => {
  await capture?.stop()
  capture = undefined
})

async function benchTool(options: ToolRegistrationOptions): Promise<CapturedTool> {
  capture = await captureRegisteredTools(options)
  const tool = capture.tools.find((candidate) => candidate.name === 'org_bench')
  if (isNullish(tool)) throw new Error('org_bench must be registered for the CEO')
  return tool
}

async function runBench(tool: CapturedTool): Promise<ToolResult> {
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `execute` is Pi's own tool signature; the harness captures the definition
  // verbatim, so this is the registered function and not a re-declaration.
  const execute = tool.execute as (
    toolCallId: string,
    params: Record<string, unknown>
  ) => Promise<ToolResult>
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  return execute('call-1', { personId: 'bo' })
}

describe('org_bench and a committed-but-unconverged bench', () => {
  test('a delivered 503 whose artifact checks out is reported as the success it is', async () => {
    const tool = await benchTool({
      people: worker('benched'),
      routes: { [BENCH_ROUTE]: { status: 503, body: CONVERGENCE_TIMEOUT_BODY } }
    })

    const result = await runBench(tool)

    expect(result.details?.ok).toBe(true)
    // `@bo`, not `bo`: a tool result is prose an agent reads, so it names the
    // person by username like every other communication surface.
    expect(text(result)).toContain('Benched @bo')
    expect(text(result)).toContain('do not repeat the bench')
    expect(result.details?.handoff).toBe('unconfirmed')
    expect(result.details?.personId).toBe('bo')
    // Non-vacuity: the 503 really did reach the client. Without this the test
    // would pass just as well against a route that answered 200.
    expect(capture?.chiefdPaths).toContain(BENCH_ROUTE)
  })

  test('the same 503 with the artifact NOT verified stays a failure', async () => {
    // The recovery is not "503 means fine". It is "the durable bench landed",
    // and the only evidence for that is the roster. A person still active after
    // a 503 is a real failure and must read as one.
    const tool = await benchTool({
      people: worker('active'),
      routes: { [BENCH_ROUTE]: { status: 503, body: CONVERGENCE_TIMEOUT_BODY } }
    })

    const result = await runBench(tool)

    expect(result.details?.ok).toBe(false)
    expect(text(result)).not.toContain('do not repeat the bench')
  })

  test('a 503 the CLIENT never receives cannot be recovered — which is why the bound is coupled', async () => {
    // The failure mode this whole family exists for, stated as a test. When the
    // route's wait outlives `DEFAULT_TIMEOUT_MS` the client aborts first and
    // raises `kind: 'timeout'` with NO status, so the branch above cannot
    // match however correct it is. Here the transport simply never gets an
    // answer it can classify as 503 — the route answers a plain 500 — and the
    // committed bench is reported as an outage.
    const tool = await benchTool({
      people: worker('benched'),
      routes: { [BENCH_ROUTE]: { status: 500, body: UNCLASSIFIED_BODY } }
    })

    const result = await runBench(tool)

    expect(result.details?.ok).toBe(false)
  })
})

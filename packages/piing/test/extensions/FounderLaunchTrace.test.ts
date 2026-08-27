/**
 * What `chiefd_launch_company` writes while it runs.
 *
 * # The defect, measured live
 *
 *     22:46:42.577  founder emits toolCall chiefd_launch_company
 *                   |  140.6 s dark: no chiefd log line, no socket to
 *                   |  CHIEFD_FOUNDER_URL, no child process
 *     22:49:03.136  chiefd logs founder.launch   <- first contact with chiefd
 *     22:49:05.780  founder.launch.ok  durationMs=2643
 *
 * chiefd's measured share of a 143.2-second launch was 2.643 seconds — 1.8%.
 * The other 98.2% ran inside this extension and left no evidence anywhere, so
 * the earlier, larger incident (4 m 34 s behind a bare spinner) was attributed
 * to daemon boot and to company create, and both guesses were wrong.
 *
 * These tests assert the property that ends the guessing: every step of the
 * tool leaves an `enter`/`exit` pair, the exit line carries the real elapsed
 * time, and the slowest step is therefore nameable from the file alone.
 */
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import founderLaunch from '@test-assets/founder-launch'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const SLUG = 'leo-capital'

const LAUNCH_RESULT = {
  slug: SLUG,
  url: 'http://127.0.0.1:8791',
  chiefPersonId: 'executive-ceo',
  session: `org-${SLUG}`
}

type Line = Record<string, unknown>

/**
 * A parsed line as a keyed record.
 *
 * `JSON.parse` returns `any`, and this package forbids type assertions, so the
 * narrowing is a real structural check with a total answer: anything that is
 * not a plain object reads as an empty line, which then fails the assertion
 * that wanted a field rather than silently satisfying it.
 */
function asRecord(value: unknown): Line {
  if (typeof value !== 'object' || !value || Array.isArray(value)) return {}
  return Object.fromEntries(Object.entries(value))
}

interface RegisteredTool {
  name: string
  execute(
    id: string,
    params: { name: string; purpose: string }
  ): Promise<{ details: { ok: boolean }; isError?: boolean }>
}

let companyDir = ''

beforeEach(() => {
  companyDir = mkdtempSync(join(tmpdir(), 'founder-launch-trace-'))
  // `vi.stubEnv` rather than a hand-rolled save/restore: the extension resolves
  // both its stream directory and its genesis endpoint from the process
  // environment, and `vi.unstubAllEnvs` puts every one of them back even when
  // an assertion throws part-way through a test.
  vi.stubEnv('ORG_LAUNCHER_ORG_DIR', companyDir)
  vi.stubEnv('CHIEFD_FOUNDER_URL', 'http://127.0.0.1:44349')
})

afterEach(() => {
  vi.unstubAllEnvs()
  vi.unstubAllGlobals()
  rmSync(companyDir, { recursive: true, force: true })
})

/**
 * Occupy the calling thread for `millis` of REAL wall-clock time.
 *
 * Not a timer, and not for convenience: `durationMs` is measured from a real
 * clock inside the trace, so the only way to prove the measurement is real —
 * rather than a constant zero that would satisfy a weaker assertion — is for
 * real time to pass.
 */
function occupy(millis: number): void {
  const until = Date.now() + millis
  while (Date.now() < until) {
    // Deliberately busy: see above.
  }
}

interface Installed {
  readonly tool: RegisteredTool
  openSession(): void
}

/**
 * Install the REAL extension and hand back its one tool plus the session
 * opener, so a test can choose whether a session was ever started.
 */
function install(): Installed {
  const handlers = new Map<string, (event: unknown, context: unknown) => void>()
  let captured: unknown
  const pi = {
    on(name: string, handler: (event: unknown, context: unknown) => void): void {
      handlers.set(name, handler)
    },
    registerTool(tool: unknown): void {
      captured = tool
    }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // A fake standing in for Pi's own `ExtensionAPI`, which this test cannot
  // implement in full and must not: the point is to drive the REAL extension,
  // and the two members it uses are the two declared above. The registered
  // tool is likewise Pi's shape, narrowed to the one method this file calls.
  founderLaunch(pi as never)
  if (!captured) throw new Error('the founder extension registered no tool')
  const tool = captured as RegisteredTool
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  return {
    tool,
    openSession(): void {
      handlers.get('session_start')?.(undefined, {})
    }
  }
}

/** Install and open a session — what every launch test needs. */
function installedTool(): RegisteredTool {
  const installed = install()
  installed.openSession()
  return installed.tool
}

function tracePath(): string {
  return join(companyDir, '.chief', 'log', 'founder-pi.jsonl')
}

function lines(): Line[] {
  return readFileSync(tracePath(), 'utf8')
    .split('\n')
    .filter((line) => line.length > 0)
    .map((line): Line => {
      const parsed: unknown = JSON.parse(line)
      return asRecord(parsed)
    })
}

function detailOf(line: Line): Line {
  return asRecord(line.detail)
}

/** The `exit` line of one step, which is where its duration lives. */
function exitOf(written: Line[], event: string): Line {
  const found = written.find((line) => line.event === event && detailOf(line).phase === 'exit')
  if (!found) throw new Error(`no exit line for ${event}`)
  return found
}

function durationOf(written: Line[], event: string): number {
  return Number(detailOf(exitOf(written, event)).durationMs)
}

/**
 * A launch stream whose delivery costs REAL wall-clock time.
 *
 * The cost used to sit on the Pi model-registry read, which was the one step
 * that ran before chiefd was reached at all. Provider/model management is
 * deleted, so the only blocking step left is the launch itself — and a fixture
 * whose cost sits on a call the product never makes proves nothing. A `failure`
 * replaces the terminal frame with chiefd's own refusal.
 */
function stubSlowLaunchResponse(readMillis: number, failure?: string): void {
  /* eslint-disable lucy/no-json-stringify */
  // Wire text for a fixture response — the same exemption every other HTTP
  // fixture in this package takes.
  const terminal = failure
    ? `event: failed\ndata: ${JSON.stringify({ code: 'lifecycle-failed', detail: failure })}\n\n`
    : `event: launched\ndata: ${JSON.stringify(LAUNCH_RESULT)}\n\n`
  const body =
    `event: phase\ndata: ${JSON.stringify({ phase: 'company-claim', slug: SLUG, detail: '' })}\n\n` +
    terminal
  /* eslint-enable lucy/no-json-stringify */
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => {
      occupy(readMillis)
      return new Response(body, {
        status: 200,
        headers: { 'content-type': 'text/event-stream' }
      })
    })
  )
}

function stubLaunchResponse(): void {
  /* eslint-disable lucy/no-json-stringify */
  // Wire text for a fixture response — the same exemption every other HTTP
  // fixture in this package takes.
  //
  // #1051 moved `/v1/founder/launch` from one JSON body to a phase STREAM, so
  // this fixture answers `text/event-stream`: a couple of phase frames and one
  // terminal `launched` frame carrying the same `LAUNCH_RESULT`. Every
  // assertion in this file is unchanged — the trace records, their durations
  // and the company naming are all still asserted exactly as before. Only the
  // shape of the wire underneath them moved.
  const body =
    `event: phase\ndata: ${JSON.stringify({ phase: 'company-claim', slug: SLUG, detail: '' })}\n\n` +
    `event: phase\ndata: ${JSON.stringify({ phase: 'durable-create', slug: SLUG, detail: '' })}\n\n` +
    `event: launched\ndata: ${JSON.stringify(LAUNCH_RESULT)}\n\n`
  /* eslint-enable lucy/no-json-stringify */
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } })
    )
  )
}

describe('a successful launch is attributable from the file alone', () => {
  it('traces every step from tool entry to the return', async () => {
    stubLaunchResponse()
    const tool = installedTool()
    const result = await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })
    expect(result.details.ok).toBe(true)

    const written = lines()
    const names = written.map((line) => line.event)
    // Extension load, then the session, then the tool. Comparing the session
    // line against the transcript's own `toolCall` timestamp measures the one
    // segment no in-process instrument can see.
    expect(names[0]).toBe('founder.trace.open')
    expect(names[1]).toBe('founder.session.start')
    for (const step of ['founder.launch', 'founder.endpoint.resolve', 'founder.chiefd.launch']) {
      expect(names).toContain(step)
    }
    // Every step that can block closed with a measured duration.
    for (const step of ['founder.launch', 'founder.endpoint.resolve', 'founder.chiefd.launch']) {
      expect(Number.isFinite(durationOf(written, step))).toBe(true)
    }
    // And the model-observation steps that are GONE stay gone. They measured
    // the provider work this tool no longer does at all; a span for a call
    // that can never fire reads, to anybody grepping the stream, as a step
    // that mysteriously stopped happening.
    for (const step of [
      'founder.bootstrap.observe',
      'founder.model.route',
      'founder.registry.error-check',
      'founder.registry.read',
      'founder.registry.refresh'
    ]) {
      expect(names).not.toContain(step)
    }
  })

  it('names the slowest step, which is the whole point of the file', async () => {
    // A cost this test controls, on the one step that can still block.
    stubSlowLaunchResponse(60)
    const tool = installedTool()
    await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })

    const written = lines()
    const slowest = durationOf(written, 'founder.chiefd.launch')
    expect(slowest).toBeGreaterThanOrEqual(50)
    // It dominates the step that only reads memory, so a reader ranks them
    // without subtracting timestamps by hand.
    expect(slowest).toBeGreaterThan(durationOf(written, 'founder.endpoint.resolve'))
    // And it is inside the launch.
    expect(durationOf(written, 'founder.launch')).toBeGreaterThanOrEqual(slowest)
  })

  it('names the company from the moment chiefd answers with a slug', async () => {
    stubLaunchResponse()
    const tool = installedTool()
    await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })

    const written = lines()
    // The launch has no company for most of its duration — that is the window
    // being measured — and one by the time it closes.
    expect(written[0]?.organization).toBe('-')
    const launchExit = exitOf(written, 'founder.launch')
    expect(launchExit.organization).toBe(SLUG)
    expect(detailOf(launchExit).ok).toBe(true)
    expect(detailOf(launchExit).slug).toBe(SLUG)
    expect(detailOf(launchExit).session).toBe(`org-${SLUG}`)
  })

  it('writes its own stream beside the Rust ones, never into chiefd.jsonl', async () => {
    stubLaunchResponse()
    const tool = installedTool()
    await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })

    expect(lines().length).toBeGreaterThan(0)
    expect(() => readFileSync(join(companyDir, '.chief', 'log', 'chiefd.jsonl'), 'utf8')).toThrow()
  })
})

describe('a refused launch is measured too', () => {
  it('records the failing step, its duration and the refusal', async () => {
    stubSlowLaunchResponse(40, "'leo-capital' already exists")
    const tool = installedTool()
    const result = await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })
    expect(result.isError).toBe(true)
    expect(result.details.ok).toBe(false)

    const written = lines()
    const failed = exitOf(written, 'founder.chiefd.launch')
    expect(failed.level).toBe('error')
    expect(durationOf(written, 'founder.chiefd.launch')).toBeGreaterThanOrEqual(30)

    const launchExit = exitOf(written, 'founder.launch')
    expect(launchExit.level).toBe('error')
    expect(detailOf(launchExit).ok).toBe(false)
    expect(String(detailOf(launchExit).error)).toContain('already exists')
  })

  it('never lets a credential in the failure text reach the file', async () => {
    stubSlowLaunchResponse(1, 'daemon probe failed: OPENROUTER_API_KEY=sk-live-abc123')
    const tool = installedTool()
    await tool.execute('call-1', { name: 'Leo Capital', purpose: 'Invest well.' })

    const raw = readFileSync(tracePath(), 'utf8')
    expect(raw).not.toContain('sk-live-abc123')
    expect(raw).toContain('[redacted]')
    // The diagnostic an operator actually reads survives the mask.
    expect(raw).toContain('daemon probe failed')
  })

  it('names its own stream before any session exists', async () => {
    // `founder.trace.open` is written at extension load rather than when a
    // session opens, so a launch that fails before `session_start` still
    // leaves a record that says where to look.
    install()

    expect(detailOf(lines()[0] ?? {}).stream).toBe(tracePath())
  })
})

/**
 * The Pi-side observability stream, asserted against the Rust one it copies.
 *
 * A measured `chiefd_launch_company` took 143.2 seconds and chiefd's own logs
 * accounted for 2.643 of them. The other 140.6 seconds ran inside a Pi
 * extension and wrote nothing anywhere, so the step could not be explained
 * from disk at all. These tests pin the two properties that make this file
 * worth writing: it is the SAME record `chiefd-log` emits, so the two streams
 * merge on timestamp, and a step that blocks says how long it blocked for on
 * the line that ends it.
 */
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { afterEach, beforeEach, describe, expect, it } from 'vitest'

import {
  CHIEFD_LOG_SCHEMA_VERSION,
  CHIEFD_LOG_TOP_LEVEL_KEYS,
  chiefdLogDirectory,
  droppedTraceLines,
  FOUNDER_TRACE_SERVICE,
  LaunchTrace,
  NO_ORGANIZATION,
  redactSecrets,
  resetTraceStreamStateForTests,
  traceMaxBytes
} from '@/extensionruntime/LaunchTrace'

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

let directory = ''

beforeEach(() => {
  resetTraceStreamStateForTests()
  directory = mkdtempSync(join(tmpdir(), 'launch-trace-'))
})

afterEach(() => {
  rmSync(directory, { recursive: true, force: true })
})

/** A trace whose clock the test drives, so a duration is an asserted number
 *  rather than whatever the machine happened to do. */
function traceWithClock(
  clock: { millis: number },
  environment: Record<string, string> = {}
): LaunchTrace {
  return new LaunchTrace({
    environment,
    directory,
    pid: 4242,
    now: () => clock.millis
  })
}

/**
 * The stream's path, asserted present.
 *
 * `LaunchTrace.path` is optional because a process with no company directory
 * and no `$HOME` has nowhere honest to write. Every test that reads bytes back
 * built its trace with a directory, so the absent case is a bug in the test
 * rather than a case to handle — this says which, once, instead of a `!` per
 * call site.
 */
function pathOf(trace: LaunchTrace): string {
  const path = trace.path
  if (!path) throw new Error('this trace names no stream to read')
  return path
}

function lines(trace: LaunchTrace): Line[] {
  return readFileSync(pathOf(trace), 'utf8')
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

describe('the log directory is the same two answers chiefd-log gives', () => {
  it('writes beside the store when the pane names a company directory', () => {
    expect(chiefdLogDirectory({ ORG_LAUNCHER_ORG_DIR: '/work/anvils', HOME: '/home/x' })).toBe(
      '/work/anvils/.chief/log'
    )
  })

  it('writes under $HOME for a box-wide process, which is a category and not a fallback', () => {
    // `chief` itself spends the minutes before any company exists with no
    // directory to write into. That is not a degraded case.
    expect(chiefdLogDirectory({ HOME: '/Users/user' })).toBe('/Users/user/.chief/log')
  })

  it('treats a blank company directory as absent rather than as the filesystem root', () => {
    expect(chiefdLogDirectory({ ORG_LAUNCHER_ORG_DIR: '  ', HOME: '/home/x' })).toBe(
      '/home/x/.chief/log'
    )
  })

  it('answers nothing when the environment names nowhere, and never invents /root', () => {
    // The deleted fourth tier was the literal `/root/.chiefd`, a directory the
    // process almost certainly cannot write on any host where $HOME is unset —
    // so it produced the same silence it existed to prevent, while looking
    // like it had an answer.
    expect(chiefdLogDirectory({})).toBe(undefined)
    expect(chiefdLogDirectory({ HOME: '   ' })).toBe(undefined)
  })

  it('drops its lines rather than writing somewhere invented, when it has nowhere to write', () => {
    const trace = new LaunchTrace({ environment: {} })
    expect(trace.path).toBe(undefined)
    // Best-effort by construction: emitting must not throw just because there
    // is no file. The console layer still carries the line.
    expect(() => trace.event('info', 'launch.start')).not.toThrow()
  })

  it('writes its own file beside the Rust streams, never into chiefd.jsonl', () => {
    const trace = new LaunchTrace({ environment: { ORG_LAUNCHER_ORG_DIR: '/work/anvils' } })
    expect(trace.path).toBe(`/work/anvils/.chief/log/${FOUNDER_TRACE_SERVICE}.jsonl`)
    expect(trace.path?.endsWith('/chiefd.jsonl')).toBe(false)
  })

  it('reads the per-stream cap from the variable the Rust sink reads', () => {
    expect(traceMaxBytes({ ORG_LOG_MAX_BYTES: '4096' })).toBe(4096)
    expect(traceMaxBytes({ ORG_LOG_MAX_BYTES: '0' })).toBe(16 * 1024 * 1024)
    expect(traceMaxBytes({ ORG_LOG_MAX_BYTES: 'nonsense' })).toBe(16 * 1024 * 1024)
    expect(traceMaxBytes({})).toBe(16 * 1024 * 1024)
  })
})

describe('the record is the one chiefd-log emits', () => {
  it('carries only the closed set of top-level keys', () => {
    const trace = traceWithClock({ millis: 1_764_000_000_000 })
    trace.event('warn', 'founder.launch', { error: 'boom' })

    const [line] = lines(trace)
    expect(line).toBeDefined()
    for (const key of Object.keys(line ?? {})) {
      expect(CHIEFD_LOG_TOP_LEVEL_KEYS).toContain(key)
    }
    expect(line?.schemaVersion).toBe(CHIEFD_LOG_SCHEMA_VERSION)
    expect(line?.service).toBe(FOUNDER_TRACE_SERVICE)
    expect(line?.level).toBe('warn')
    expect(line?.event).toBe('founder.launch')
    expect(line?.pid).toBe(4242)
    expect(detailOf(line ?? {}).error).toBe('boom')
  })

  it('timestamps every line to the millisecond', () => {
    const trace = traceWithClock({ millis: 1_764_000_000_000 })
    trace.event('info', 'founder.session.start')
    const at = String(lines(trace)[0]?.at)
    expect(at).toHaveLength(24)
    expect(at.endsWith('Z')).toBe(true)
    expect(at).toContain('T')
    expect(at).toContain('.')
  })

  it('names the absence of a company rather than omitting it', () => {
    const trace = traceWithClock({ millis: 1 })
    trace.event('info', 'founder.session.start')
    expect(lines(trace)[0]?.organization).toBe(NO_ORGANIZATION)
  })

  it('names the company once chiefd has answered with a slug', () => {
    const trace = traceWithClock({ millis: 1 })
    const launch = trace.step('founder.launch')
    trace.nameCompany('leo-capital')
    launch.close()
    const written = lines(trace)
    // The step opened before the slug existed — which is the point: the launch
    // has no company for most of its duration.
    expect(written[0]?.organization).toBe(NO_ORGANIZATION)
    expect(written[1]?.organization).toBe('leo-capital')
  })

  it('keeps sequence numbers gapless so a lost line is detectable', () => {
    const trace = traceWithClock({ millis: 1 })
    for (let index = 0; index < 5; index += 1) trace.event('info', 'tick')
    expect(lines(trace).map((line) => line.seq)).toEqual([0, 1, 2, 3, 4])
  })

  it('omits detail entirely when a line has nothing to say', () => {
    const trace = traceWithClock({ millis: 1 })
    trace.event('info', 'founder.session.shutdown', {})
    expect(lines(trace)[0]?.detail).toBeUndefined()
  })

  it('cannot have its framing broken by a value carrying quotes or newlines', () => {
    const trace = traceWithClock({ millis: 1 })
    trace.event('info', 'founder.launch', { note: 'has "quotes",\n a brace } and {' })
    const raw = readFileSync(pathOf(trace), 'utf8')
    expect(raw.trimEnd().split('\n')).toHaveLength(1)
    expect(detailOf(lines(trace)[0] ?? {}).note).toBe('has "quotes",\n a brace } and {')
  })
})

describe('a step says how long it took on the line that closes it', () => {
  it('writes an enter line and an exit line carrying durationMs', () => {
    const clock = { millis: 1_000 }
    const trace = traceWithClock(clock)
    const step = trace.step('founder.registry.refresh', { call: 1 })
    clock.millis += 140_600
    const measured = step.close({ modelCount: 37 })

    expect(measured).toBe(140_600)
    const written = lines(trace)
    expect(written).toHaveLength(2)
    expect(written[0]?.event).toBe('founder.registry.refresh')
    expect(detailOf(written[0] ?? {}).phase).toBe('enter')
    expect(detailOf(written[0] ?? {}).durationMs).toBeUndefined()
    expect(detailOf(written[1] ?? {}).phase).toBe('exit')
    expect(detailOf(written[1] ?? {}).durationMs).toBe(140_600)
    expect(detailOf(written[1] ?? {}).modelCount).toBe(37)
    // The fields stated at open survive to the exit line, so one grep reads
    // the whole step.
    expect(detailOf(written[1] ?? {}).call).toBe(1)
  })

  it('carries a field recorded after the step opened onto its exit line', () => {
    const clock = { millis: 0 }
    const trace = traceWithClock(clock)
    const step = trace.step('founder.launch')
    step.record({ slug: 'leo-capital' })
    step.close()
    expect(detailOf(lines(trace)[1] ?? {}).slug).toBe('leo-capital')
  })

  it('measures a step that failed, because that is the one being read about', () => {
    const clock = { millis: 0 }
    const trace = traceWithClock(clock)
    const step = trace.step('founder.chiefd.launch')
    clock.millis += 2_643
    step.fail(new Error('ChiefD refused the launch (503).'))

    const exit = lines(trace)[1]
    expect(exit?.level).toBe('error')
    expect(detailOf(exit ?? {}).durationMs).toBe(2_643)
    expect(detailOf(exit ?? {}).error).toBe('ChiefD refused the launch (503).')
  })

  it('closes exactly once, so a double close cannot invent a second exit', () => {
    const trace = traceWithClock({ millis: 0 })
    const step = trace.step('founder.launch')
    step.close()
    step.close()
    expect(lines(trace)).toHaveLength(2)
  })

  it('measure closes on both outcomes and re-throws the failure', async () => {
    const clock = { millis: 0 }
    const trace = traceWithClock(clock)
    const value = await trace.measure('founder.bootstrap.observe', async () => {
      clock.millis += 11
      return 'observed'
    })
    expect(value).toBe('observed')

    await expect(
      trace.measure('founder.chiefd.launch', async () => {
        clock.millis += 7
        throw new Error('refused')
      })
    ).rejects.toThrow('refused')

    const written = lines(trace)
    expect(written.map((line) => line.event)).toEqual([
      'founder.bootstrap.observe',
      'founder.bootstrap.observe',
      'founder.chiefd.launch',
      'founder.chiefd.launch'
    ])
    expect(detailOf(written[1] ?? {}).durationMs).toBe(11)
    expect(written[3]?.level).toBe('error')
    expect(detailOf(written[3] ?? {}).durationMs).toBe(7)
  })

  it('measureSync times a step that does not await, so it can be ruled out', () => {
    const clock = { millis: 0 }
    const trace = traceWithClock(clock)
    const value = trace.measureSync('founder.endpoint.resolve', () => {
      clock.millis += 1
      return 'http://127.0.0.1:44349'
    })
    expect(value).toBe('http://127.0.0.1:44349')
    expect(detailOf(lines(trace)[1] ?? {}).durationMs).toBe(1)
  })
})

describe('a credential never reaches the file', () => {
  it('masks credential-named assignments and bare credential prefixes', () => {
    // The three names this environment actually carries.
    expect(redactSecrets('OPENROUTER_API_KEY=sk-live-abc123')).toBe('OPENROUTER_API_KEY=[redacted]')
    expect(redactSecrets('XCOM_API_KEY: xk-9')).toBe('XCOM_API_KEY: [redacted]')
    expect(redactSecrets('TRIBES_SSH_PUBLIC_KEY=ssh-ed25519 AAAA')).toBe(
      'TRIBES_SSH_PUBLIC_KEY=[redacted] AAAA'
    )
    expect(redactSecrets('failed for sk-live-1234567890')).toBe('failed for [redacted]')
    expect(redactSecrets('ghp_deadbeef rejected')).toBe('[redacted] rejected')
  })

  it('preserves ordinary diagnostics, because operators read them', () => {
    expect(redactSecrets("runtime: can't find session: cobalt")).toBe(
      "runtime: can't find session: cobalt"
    )
    expect(redactSecrets('exit status 1')).toBe('exit status 1')
  })

  it('redacts every line, not just the first', () => {
    expect(redactSecrets('line one\nAPI_KEY=sk-secret\nline three')).toBe(
      'line one\nAPI_KEY=[redacted]\nline three'
    )
  })

  it('applies the mask to the event name, to nested detail and to a thrown error', () => {
    const trace = traceWithClock({ millis: 0 })
    trace.event('error', 'probe failed with sk-live-abc123', {
      environment: { OPENROUTER_API_KEY: 'sk-live-abc123' },
      argv: ['--token=sk-live-abc123']
    })
    trace.step('founder.chiefd.launch').fail(new Error('spawn failed: XCOM_API_KEY=xk-9'))

    const raw = readFileSync(pathOf(trace), 'utf8')
    expect(raw).not.toContain('sk-live-abc123')
    expect(raw).not.toContain('xk-9')
    expect(raw).toContain('[redacted]')
    // The surrounding diagnostic survives.
    expect(raw).toContain('spawn failed')
  })
})

describe('the stream is bounded and can never break the turn it observes', () => {
  it('rotates at the cap and keeps exactly one previous generation', () => {
    const trace = new LaunchTrace({
      environment: { ORG_LOG_MAX_BYTES: '4096' },
      directory,
      pid: 1,
      now: () => 0
    })
    const filler = 'x'.repeat(200)
    for (let index = 0; index < 200; index += 1) trace.event('info', 'tick', { filler })

    expect(statSync(pathOf(trace)).size).toBeLessThanOrEqual(4096 + 512)
    expect(statSync(`${pathOf(trace)}.1`).size).toBeLessThanOrEqual(4096 + 512)
    // A third generation must never accumulate.
    expect(() => statSync(`${pathOf(trace)}.2`)).toThrow()
  })

  it('drops a line it cannot write instead of throwing at the caller', () => {
    // The log path itself is a directory, so the append cannot succeed.
    mkdirSync(join(directory, `${FOUNDER_TRACE_SERVICE}.jsonl`), { recursive: true })
    const trace = traceWithClock({ millis: 0 })
    const before = droppedTraceLines()
    expect(() => trace.event('info', 'founder.session.start')).not.toThrow()
    expect(droppedTraceLines()).toBeGreaterThan(before)
  })
})

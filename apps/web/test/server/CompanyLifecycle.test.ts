// Booting and stopping a company through chiefd's own resident lifecycle
// surface.
//
// # Why the failure shapes differ, and why that is the whole test
//
// Boot is a STREAM. The response headers have already gone out by the time
// chiefd refuses — a launch runs for minutes — so there is no status code left
// to change, and a refusal must arrive as a terminal `failed` FRAME. Tearing
// the connection down instead would leave `BootPhaseConsole` showing the last
// phase it saw, forever, with no reason: the browser's lifecycle reader ends on
// a terminal frame and on nothing else.
//
// Stop is NOT a stream — chiefd answers it with one object — so its refusal is
// a thrown `RouteRefusalError` with a 409, which is the status a caller acts on.
//
// Both carry chiefd's OWN code and detail verbatim. Replacing either with
// "failed" would give the operator a second vocabulary for chiefd's answer,
// which is the standing defect this whole layer keeps repairing.
import {
  type CompanyLaunchResult,
  type CompanyLifecyclePhase,
  CompanyLifecycleRefusalError
} from '@chief/chiefing'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const boot = vi.fn()
const stop = vi.fn()

// The host URL is `common/Env`'s to resolve — the only module allowed to read
// the environment. Stubbed so this suite is about the lifecycle and not about
// which variable names a box happens to have set.
vi.mock('@/common/Env', () => ({
  chiefdHostUrl: () => 'http://127.0.0.1:8789',
  beacondUrl: () => 'http://127.0.0.1:6969'
}))

// The company DIRECTORY translation, which is the one slug-keyed hop left in
// this app: `chief host`'s lifecycle wire still names a company by slug, so
// boot and stop resolve the key back to a display name through the directory.
// Stubbed rather than driven through a fake beacond because THAT resolution is
// `CompanyDirectory`'s own test's subject; here it is a precondition.
vi.mock('@/server/CompanyDirectory', () => ({
  companySummary: (companyKey: string) =>
    Promise.resolve(
      companyKey === COMPANY_KEY
        ? {
            key: COMPANY_KEY,
            dir: '/work/acme',
            slug: 'acme',
            status: 'running' as const,
            chiefd: { healthy: true }
          }
        : undefined
    )
}))

/** The company these tests address. `acme` is its display word; this is its
 * identity. */
const COMPANY_KEY = '0123456789ab'

// The REAL `CompanyLifecycleRefusalError`, because `stopCompany` and
// `bootCompany` both branch on `instanceof`. A stubbed error class would let a
// mislabelled failure pass as a refusal, which is precisely the distinction
// under test.
vi.mock('@chief/chiefing', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@chief/chiefing')>()
  return {
    ...actual,
    // No constructor: the host URL is `common/Env`'s to resolve and this fake
    // has nothing to do with it. `new CompanyLifecycleClient({hostUrl})` still
    // constructs — the argument is simply ignored, which is the honest stand-in
    // for "the address plays no part in what is under test".
    CompanyLifecycleClient: class {
      boot(slug: string): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
        return boot(slug)
      }
      async stop(slug: string): Promise<unknown> {
        return stop(slug)
      }
    }
  }
})

const { bootCompany, stopCompany } = await import('@/server/CompanyLifecycle')

// The key and the slug are deliberately different strings: a fixture that
// reused one could not tell an addressed-by-key caller from an addressed-by-
// slug one, which is the exact confusion this frame now exists to remove.
const LAUNCHED: CompanyLaunchResult = {
  slug: 'acme',
  key: '4d0e2ed2cec4',
  dir: '/work/acme',
  url: 'http://127.0.0.1:52341',
  chiefPersonId: 'person-ceo',
  session: 'acme'
}

/** chiefd's generator: some phases, then a launch result. */
function launching(
  phases: readonly CompanyLifecyclePhase[],
  result: CompanyLaunchResult = LAUNCHED
): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
  return (async function* generate(): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
    for (const phase of phases) yield phase
    return result
  })()
}

/** chiefd's generator when it refuses partway through. */
function refusing(
  phases: readonly CompanyLifecyclePhase[],
  error: Error
): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
  return (async function* generate(): AsyncGenerator<CompanyLifecyclePhase, CompanyLaunchResult> {
    for (const phase of phases) yield phase
    throw error
  })()
}

/** The SSE bytes this route actually writes, as text. */
async function drain(stream: ReadableStream<Uint8Array>): Promise<string> {
  const decoder = new TextDecoder()
  const reader = stream.getReader()
  let text = ''
  for (;;) {
    const chunk = await reader.read()
    if (chunk.done) return text
    text += decoder.decode(chunk.value)
  }
}

/** Every `event:` name in order — the browser's whole protocol here. */
function events(wire: string): string[] {
  return wire
    .split('\n')
    .filter((line) => line.startsWith('event: '))
    .map((line) => line.slice('event: '.length))
}

beforeEach(() => {
  boot.mockReset()
  stop.mockReset()
})

describe('bootCompany', () => {
  it('narrates every phase and ends on a terminal booted frame', async () => {
    boot.mockReturnValue(
      launching([
        { phase: 'company-daemon-start', slug: 'acme', detail: 'starting company daemon' },
        { phase: 'chief-start', slug: 'acme', detail: 'starting CEO' }
      ])
    )

    const wire = await drain(bootCompany(COMPANY_KEY))

    expect(events(wire)).toEqual(['phase', 'phase', 'booted'])
    // The phase is carried whole. `BootPhaseConsole` shows the detail, and a
    // frame that dropped it would narrate a launch with no content.
    expect(wire).toContain('"phase":"chief-start"')
    expect(wire).toContain('"detail":"starting CEO"')
    expect(wire).toContain('"slug":"acme"')
  })

  it('turns a refusal into a failed FRAME rather than a torn-down stream', async () => {
    // The headers are already out. A rejected stream leaves the console showing
    // the last phase it saw, forever, with no reason — which is worse than the
    // failure it is reporting.
    boot.mockReturnValue(
      refusing(
        [{ phase: 'company-daemon-start', slug: 'acme', detail: 'starting company daemon' }],
        new CompanyLifecycleRefusalError({
          code: 'company-already-running',
          detail: 'acme is already up on 127.0.0.1:52341'
        })
      )
    )

    const wire = await drain(bootCompany(COMPANY_KEY))

    expect(events(wire)).toEqual(['phase', 'failed'])
    // chiefd's own code and detail, verbatim. Replacing them with "failed"
    // would give the operator a second vocabulary for chiefd's answer.
    expect(wire).toContain('"code":"company-already-running"')
    expect(wire).toContain('acme is already up on 127.0.0.1:52341')
  })

  it('reports a failure that is NOT a refusal under its own code', async () => {
    // A connection reset is not chiefd declining. Labelling it
    // `company-already-running` — or any chiefd code — would send an operator
    // looking for a company that is not there.
    boot.mockReturnValue(refusing([], new Error('connect ECONNREFUSED 127.0.0.1:8789')))

    const wire = await drain(bootCompany(COMPANY_KEY))

    expect(events(wire)).toEqual(['failed'])
    expect(wire).toContain('"code":"lifecycle-failed"')
    expect(wire).toContain('ECONNREFUSED')
  })

  it('closes the stream after the terminal frame, whichever one it was', async () => {
    // `drain` returning at all is the assertion: the browser's reader ends on a
    // terminal frame, and a stream left open would hang the console.
    boot.mockReturnValue(launching([]))
    expect(events(await drain(bootCompany(COMPANY_KEY)))).toEqual(['booted'])

    boot.mockReturnValue(refusing([], new Error('boom')))
    expect(events(await drain(bootCompany(COMPANY_KEY)))).toEqual(['failed'])
  })
})

describe('stopCompany', () => {
  it('reports both halves of what stopping actually did', async () => {
    // A company whose session was killed but whose daemon survived is NOT
    // stopped, and a single `stopped: true` would hide exactly that.
    stop.mockResolvedValue({
      slug: 'acme',
      mode: 'orphan-session-stopped',
      session: 'acme',
      sessionStopped: true,
      daemonStopped: false
    })

    await expect(stopCompany(COMPANY_KEY)).resolves.toEqual({
      slug: 'acme',
      mode: 'orphan-session-stopped',
      sessionStopped: true,
      daemonStopped: false
    })
    expect(stop).toHaveBeenCalledWith('acme')
  })

  it('maps a refusal to a 409 carrying chiefd’s own code', async () => {
    // Not a stream, so there IS a status left to set — and a refusal is the
    // caller's problem to act on rather than an upstream fault.
    stop.mockRejectedValue(
      new CompanyLifecycleRefusalError({
        code: 'company-not-running',
        detail: 'acme has no supervised session'
      })
    )

    await expect(stopCompany(COMPANY_KEY)).rejects.toMatchObject({
      status: 409,
      code: 'company-not-running',
      message: 'acme has no supervised session'
    })
  })

  it('lets a non-refusal through rather than calling it a conflict', async () => {
    stop.mockRejectedValue(new Error('connect ECONNREFUSED 127.0.0.1:8789'))

    const failure: unknown = await stopCompany(COMPANY_KEY).catch((error: unknown) => error)

    expect(failure).toBeInstanceOf(Error)
    expect(failure).not.toMatchObject({ status: 409 })
  })
})

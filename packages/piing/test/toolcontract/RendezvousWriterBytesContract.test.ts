/**
 * THE REAL WRITER'S BYTES, READ BY THE REAL PARSER.
 *
 * # The outage this exists for
 *
 * `host_primitives::rendezvous` (Rust) WRITES `<dir>/.chief/run/daemon.json`.
 * `parseDaemonRendezvous` (TypeScript) READS it — reached in every person's
 * pane by `organization-intercom.ts` and `team-ui.ts`. Two programs, one file.
 *
 * On 2026-08-26 the daemon began publishing an additive `build` field. The
 * reader refused the whole record, Pi exited 1, and every person in a live
 * company crash-looped — while CI stayed green throughout.
 *
 * # Why CI was green, which is the thing this file fixes
 *
 * Nothing anywhere fed the parser bytes a real chiefd had written.
 * `CompanyRendezvous.ts`'s `publishDaemonRendezvous` hand-authors exactly the
 * four fields the reader consumes; its comment says the file is written "where
 * the daemon writes it" — WHERE, not WHAT. So the writer under test and the
 * reader under test were the same program, agreeing with itself about a foreign
 * surface.
 *
 * # Why a DECLARATION parity test cannot replace this
 *
 * `RendezvousWireParity.test.ts` parses `rendezvous.rs` and compares the field
 * set it DECLARES against the reader. That is a good, cheap drift catcher and
 * it is not this, because the declaration and the bytes **already disagree
 * today, in this exact record**:
 *
 *     #[serde(default, skip_serializing_if = "Option::is_none")]
 *     pub build: Option<ReportedBuild>,
 *
 * A source parse says `build` is declared. serde omits it whenever the identity
 * could not be measured. Three readers of one declaration can agree with each
 * other and all be wrong about what is on disk.
 *
 * # Why this is a TOOLCONTRACT suite and not a unit test
 *
 * It needs a daemon that actually publishes, and only the full one does.
 * `startCompanyDaemon` boots `chiefd run --serve-only`, which returns at
 * `run.rs:2957` into `serve_only_snapshot` — BEFORE the only
 * `rendezvous::publish` call in the file, at `run.rs:3513`. **Serve-only never
 * writes a rendezvous, structurally**, so the cheap harness cannot produce the
 * bytes at all. That is the real reason this seam went untested for so long: it
 * was unreachable from the harness every suite uses, not overlooked.
 *
 * It also deliberately installs NO tool surface.
 * `installOrganizationToolSurface` REPUBLISHES the rendezvous to re-point it at
 * its proxy — legitimate there, and fatal here, because it would replace the
 * very bytes under test with the hand-authored imitation.
 */
import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { parseDaemonRendezvous } from '@chief/chiefing'
import type { TmuxHostedCompany } from '@chief/testing'
import { assertChiefdBinaryBuilt, startTmuxHostedCompany } from '@chief/testing'
// NOT from the `@chief/chiefing` barrel: that export is not a function at
// runtime, and eslint is clean either way. AGENTS.md records this exact trap;
// every sibling toolcontract suite imports it from here.
import { isNullish } from '@test/support/Nullish'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

const REPO_ROOT = join(import.meta.dirname, '..', '..', '..', '..')
const SLUG = 'rendezvous-writer-bytes'
const BOOT_TIMEOUT_MS = 120_000

let company: TmuxHostedCompany | undefined

function assertTmuxAvailable(): void {
  try {
    execFileSync('tmux', ['-V'], { stdio: 'ignore' })
  } catch {
    throw new Error(
      'the rendezvous writer-bytes contract needs tmux: only a FULL chiefd run ' +
        'publishes a rendezvous, and --serve-only returns before the publish latch'
    )
  }
}

/**
 * The daemon publishes at the same latch that binds its listener, so a
 * readiness poll can beat the file by milliseconds. Bounded, and it REFUSES
 * with what the absence would mean rather than a bare ENOENT — which reads as
 * "the daemon publishes no rendezvous" and is the wrong conclusion.
 */
function readWhenPublished(path: string): string {
  const deadline = Date.now() + 15_000
  for (;;) {
    if (existsSync(path)) return readFileSync(path, 'utf8')
    if (Date.now() > deadline) {
      throw new Error(
        `the daemon never published ${path}. Either it no longer writes a rendezvous — in ` +
          'which case every client that finds a company by standing in its directory is broken, ' +
          'and every pane that reads it is too — or the publish moved off the bind latch and ' +
          'this wait looks too early. It is not the serve-only case: this suite boots the full daemon.'
      )
    }
    execFileSync('sleep', ['0.1'])
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && !isNullish(value) && !Array.isArray(value)
}

describe('the daemon rendezvous is read from the writer, not from a fixture', () => {
  beforeAll(async () => {
    assertTmuxAvailable()
    assertChiefdBinaryBuilt(REPO_ROOT)
    // NO tool surface, deliberately — see the header. Genesis is not needed
    // either: the rendezvous is published at the listener's bind latch, long
    // before any company content exists.
    company = await startTmuxHostedCompany({ slug: SLUG, repoRoot: REPO_ROOT })
  }, BOOT_TIMEOUT_MS)

  afterAll(async () => {
    await company?.stop()
  })

  it('parses the rendezvous a real chiefd wrote, unmodified', () => {
    const live = company
    if (isNullish(live)) throw new Error('the company did not boot')

    // Read as BYTES from where chiefd put them. Never republished, never
    // re-encoded, never round-tripped through our own model — re-serializing
    // here would reintroduce exactly the gap this file exists to close.
    const bytes = readWhenPublished(join(live.dir, '.chief', 'run', 'daemon.json'))
    const body: unknown = JSON.parse(bytes)

    const parsed = parseDaemonRendezvous(body, live.dir)

    expect(parsed.dir).toBe(live.dir)
    expect(parsed.key).toBe(live.companyKey)
    expect(parsed.url).toBe(live.url)
    expect(parsed.pid).toBeGreaterThan(0)

    // NON-VACUITY. This suite's whole claim is that the reader survives what
    // the writer writes BEYOND the four fields it consumes. If the daemon ever
    // stops writing any of those, the assertions above still pass while proving
    // nothing — so refuse instead of reporting a success that has gone hollow.
    //
    // It goes red the day `build` stops being written, with a message that
    // explains itself. That is the point, not a fragility.
    const written = isRecord(body) ? Object.keys(body) : []
    expect(
      written.length,
      `the daemon wrote only [${written.join(', ')}] — this suite can no longer demonstrate ` +
        'that the reader tolerates an unmodeled field, because the writer emits none. Either ' +
        'the rendezvous lost a field, or this assertion needs a different subject.'
    ).toBeGreaterThan(4)

    // And the tolerated field is not silently adopted: the parser returns what
    // it consumes and nothing more.
    expect(Object.keys(parsed).sort()).toEqual(['dir', 'key', 'pid', 'url'])
  })
})

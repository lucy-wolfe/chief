import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { withoutComments } from '@test/support/TypeScriptSource'
import { describe, expect, test } from 'vitest'

// #751/G9 Step 0 — the dead lock-busy retry ladder.
//
// `organization-intercom.ts` carried `isPreMutationLauncherLockBusyDiagnostic`:
// eight regexes matched against a launcher subprocess's stderr to decide
// whether a failure was lock contention worth retrying. Every producer of those
// eight strings has since been deleted — the `.org.lock`/`.runtime.lock`
// file-mutex family (removed with the runtime-lifecycle port) and the SQL lease
// family (`tmux_writer_lease`, removed by #751/P2). So the predicate answered
// false for every input the live tree can produce, its ladder never took the
// retry branch, and the two `status: "busy"` tool results plus three card arms
// it gated were unreachable.
//
// The predecessor guard (`tests/busy-vocabulary-hygiene.test.ts`, now parked
// and unrunnable — it imports a `src/organization/` tree that no longer exists)
// asserted the OPPOSITE property and passed anyway, because its corpus included
// the classifier file itself: the regex literal `.../^Organization '[^']+' is
// busy; retry the staffing change$/` contains the very fragment it searched
// for, so the eight patterns were their own proof of liveness. That is the
// precise way this rotted, and it is why the corpus below EXCLUDES nothing and
// the assertion is the other direction: no production source, in either
// language, may contain any of the eight strings at all. A classifier and its
// throw site now have to come back together or not at all.

const REPO_ROOT = fileURLToPath(new URL('../../..', import.meta.url))

/** The eight refusal strings the deleted classifier matched, as literal
 * fragments (slug-independent, so a `format!`/template producer is caught as
 * readily as a constant one). */
const RETIRED_LOCK_BUSY_FRAGMENTS = [
  'is busy; retry the staffing change',
  'supervision is busy; retry',
  'runtime is busy; retry the lifecycle command',
  'supervisor state is busy; retry the lifecycle command',
  'session maintenance is busy; retry',
  'already has a unit removal in progress; retry it',
  'is already being removed; retry later',
  'is being created or removed; retry later'
] as const

/** Live production source only. `tests/` at the repo root is the parked
 * bun:test corpus (`docs/testing/parked-suite-triage.json`) — reference
 * material with a written disposition per file, not shipping code, and it is
 * neither run nor typechecked; it still names these strings and must not be
 * read as a producer. Test files under the live roots are excluded for the
 * same reason a test is allowed to name a string it asserts about. */
const PRODUCTION_ROOTS = [
  // `apps/cli/src` sat here until P3 deleted the TypeScript CLI outright; the
  // operator surface is the chiefd binary, already covered by the crates root
  // below.
  ['apps', 'chiefd', 'crates'],
  ['apps', 'web', 'src'],
  ['packages', 'piing', 'src'],
  ['packages', 'piing', 'extensions'],
  ['packages', 'chiefing', 'src']
] as const

const SKIP_DIRECTORIES = new Set(['node_modules', 'dist', 'target', 'tests', '.turbo'])
const SOURCE_EXTENSIONS = ['.ts', '.rs']

function isTestFile(name: string): boolean {
  return name.endsWith('.test.ts') || name.endsWith('.test.rs') || name === 'tests.rs'
}

function collectSourceFiles(directory: string, into: string[]): void {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (SKIP_DIRECTORIES.has(entry.name)) continue
      collectSourceFiles(join(directory, entry.name), into)
      continue
    }
    if (!SOURCE_EXTENSIONS.some((extension) => entry.name.endsWith(extension))) continue
    if (isTestFile(entry.name)) continue
    into.push(join(directory, entry.name))
  }
}

function productionSourceFiles(): string[] {
  const files: string[] = []
  for (const segments of PRODUCTION_ROOTS) collectSourceFiles(join(REPO_ROOT, ...segments), files)
  return files
}

const INTERCOM = readFileSync(
  join(REPO_ROOT, 'packages/piing/extensions/organization-intercom.ts'),
  'utf8'
)

// The tombstones this deletion left behind quote the very shapes asserted
// absent below — a guard that reads its own explanation as a violation is
// worse than no guard, so the shape assertions run against code only.
const INTERCOM_CODE = withoutComments(INTERCOM)

describe('DeadLockBusyVocabulary (#751/G9-S0)', () => {
  const files = productionSourceFiles()

  test('the corpus is real — a vacuity floor, not an inventory', () => {
    // If a tree move empties one of the roots above, this guard must go red
    // rather than pass by scanning nothing. Well below the true count.
    expect(files.length).toBeGreaterThan(200)
    expect(files.some((file) => file.endsWith('organization-intercom.ts'))).toBe(true)
    expect(files.some((file) => file.endsWith('.rs'))).toBe(true)
  })

  test('no production source in either language produces a retired lock-busy refusal', () => {
    const offenders: string[] = []
    for (const file of files) {
      const source = readFileSync(file, 'utf8')
      for (const fragment of RETIRED_LOCK_BUSY_FRAGMENTS) {
        if (!source.includes(fragment)) continue
        offenders.push(`${file.slice(REPO_ROOT.length)}: ${fragment}`)
      }
    }
    expect(
      offenders,
      'A retired lock-busy refusal string is back in production source. The classifier that ' +
        'retried these (isPreMutationLauncherLockBusyDiagnostic) was DELETED in #751/G9-S0 ' +
        'because nothing produced them; re-adding a producer without re-adding a retry policy ' +
        'means the refusal is surfaced to an agent as a hard failure. Decide deliberately: ' +
        'either word the refusal differently, or put the retry policy in chiefd next to the ' +
        'refusal, and update this guard to say so.'
    ).toEqual([])
  })

  test('the classifier, its ladder, and its queue seam are gone from the intercom', () => {
    expect(INTERCOM_CODE).not.toContain('isPreMutationLauncherLockBusyDiagnostic')
    expect(INTERCOM_CODE).not.toContain('LAUNCHER_LOCK_RETRY_BASE_DELAYS_MS')
    expect(INTERCOM_CODE).not.toContain('launcherLockRetryDelayMs')
    expect(INTERCOM_CODE).not.toContain('lockRetryDelayMs')
    expect(INTERCOM_CODE).not.toContain('isOrganizationBusyError')
  })

  test('no tool result mints a "busy" status, and no card arm reads one', () => {
    // chiefd DOES have a live busy refusal (`ChiefdError::Busy` -> HTTP 503),
    // but it reaches this file as a `ChiefdUnavailableError` and degrades
    // through `transientDegradeMessage` — never as `details.status`. A
    // `status: "busy"` here would therefore be a second, client-side authority
    // on contention, which is what G9 exists to remove.
    expect(INTERCOM_CODE).not.toMatch(/status:\s*["']busy["']/)
    expect(INTERCOM_CODE).not.toMatch(/status\s*===\s*["']busy["']/)
  })

  test('BOTH ladders are gone: the lock-busy one, and the subprocess that held the other', () => {
    // Step 0 deleted the lock-busy ladder and left `runChecked` with exactly
    // one backoff site — the transient-transport one — which this test counted.
    // The transport deletion took `runChecked` and `waitForLauncherRetry` with
    // it, so there is no backoff site in this file to count at all now. The
    // assertion is the absence, not a smaller number: a `waitForLauncherRetry`
    // back in code would mean a subprocess came back to be retried.
    expect(INTERCOM_CODE).not.toContain('waitForLauncherRetry')
    expect(INTERCOM_CODE).not.toContain('runChecked')

    // The CLASSIFIER survives the transport and must not be deleted with it.
    // `withTransientReadRetryAsync` — the boot/read retry ladder — is now its
    // only reader, and it still reaches for the same two delays. The delays are
    // asserted as a literal because that is the fact a rewrite would silently
    // change.
    expect(INTERCOM_CODE).toContain('TRANSIENT_TRANSPORT_RETRY_DELAYS_MS = [150, 400] as const;')
    expect(INTERCOM_CODE).toContain('export function isTransientTransportFailure(')
    expect(INTERCOM_CODE).toContain('export async function withTransientReadRetryAsync<T>(')
    expect(INTERCOM_CODE).toContain(
      'delaysMs: readonly number[] = TRANSIENT_TRANSPORT_RETRY_DELAYS_MS'
    )
  })
})

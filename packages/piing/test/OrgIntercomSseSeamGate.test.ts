/**
 * Regression guard for the #262 real-subprocess-spawn regression: SSE-C2
 * (`packages/piing/extensions/organization-intercom.ts`) gates real `SseWatcher`
 * construction on the SAME `pollIntervalMs > 0` check as the fallback-floor
 * timer (see `pollIntervalMs ? (options.createSseWatcher ?? (real
 * SseWatcher))(...) : undefined`). A test that passes a nonzero
 * `pollIntervalMs` (commonly `1` or a small value, to drive the floor timer
 * fast) WITHOUT also providing `createSseWatcher` (or `spawnReader`)
 * therefore constructs a REAL `SseWatcher`, which spawns a real `curl -sN`
 * child process in its constructor — exactly what broke 3 pre-existing
 * `tests/org-intercom.test.ts` tests under fleet CPU contention (tight
 * ~500ms/~50ms real-wall-clock budgets colliding with real subprocess
 * spawn overhead) once #262 landed. A 15th, un-flagged occurrence of the
 * same gap was found in `tests/org-mailbox-company-action-freeze.test.ts`
 * by writing THIS gate — see `DECISIONS.md`.
 *
 * This gate is a structural (brace-balanced) scan of every
 * `installOrganizationIntercom(...)` call site across every tracked test
 * file, not a line-proximity heuristic — it stays correct regardless of how
 * an options object is formatted or reordered. Unlike the #265 AC-2
 * poll-conversion gate (`sse-poll-conversion-grep-gate.test.ts`), this one
 * is NOT `describe.skip`'d: every current call site already carries the
 * seam (fixed as part of this same pass), so it is a real, enforced,
 * currently-green regression test from the moment it lands — guarding
 * against a FUTURE test adding `pollIntervalMs: <nonzero>` without the
 * seam and silently reintroducing real subprocess spawning.
 */

import { spawnSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { expect, test } from 'vitest'

const REPOSITORY_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')

/**
 * Extracts the full, balanced-parenthesis argument-list text of a call
 * starting right after its opening `(`, skipping over parens that appear
 * inside string/template literals so a stray `)` in a fixture string can't
 * truncate the scan early.
 */
function extractCallArgs(text: string, startIndex: number): string {
  let depth = 1
  let i = startIndex
  let inString: string | null = null
  while (i < text.length && depth > 0) {
    const ch = text[i]
    if (inString) {
      if (ch === '\\') {
        i += 2
        continue
      }
      if (ch === inString) inString = null
      i += 1
      continue
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      inString = ch
      i += 1
      continue
    }
    if (ch === '(') depth += 1
    else if (ch === ')') depth -= 1
    i += 1
  }
  return text.slice(startIndex, Math.max(startIndex, i - 1))
}

test('every nonzero-pollIntervalMs installOrganizationIntercom() call provides the createSseWatcher/spawnReader seam', () => {
  const listed = spawnSync('git', ['-C', REPOSITORY_ROOT, 'ls-files'], { encoding: 'utf8' })
  expect(listed.status).toBe(0)
  const files = listed.stdout.split('\n').filter((f) => f.endsWith('.test.ts'))
  expect(files.length).toBeGreaterThan(50) // the scan must actually be scanning the repo

  const offenders: string[] = []
  const callPattern = /installOrganizationIntercom\(/g
  for (const file of files) {
    let text: string
    try {
      text = readFileSync(resolve(REPOSITORY_ROOT, file), 'utf8')
    } catch {
      continue
    }
    for (const match of text.matchAll(callPattern)) {
      const startIndex = (match.index ?? 0) + match[0].length
      const args = extractCallArgs(text, startIndex)
      const pollMatch = args.match(/pollIntervalMs\s*:\s*(\d+)/)
      // No explicit pollIntervalMs: the production default legitimately spawns
      // real SSE and is outside this gate's test-fixture scope.
      if (!pollMatch) continue
      const ms = Number(pollMatch[1])
      // Zero constructs no SseWatcher — the compatibility contract covered by
      // this file's sibling suite.
      if (ms === 0) continue
      const hasSeam = args.includes('createSseWatcher') || args.includes('spawnReader')
      if (!hasSeam) {
        const line = text.slice(0, match.index).split('\n').length
        offenders.push(
          `${file}:${line} (pollIntervalMs: ${ms}, no createSseWatcher/spawnReader seam)`
        )
      }
    }
  }
  const failureMessage =
    'these installOrganizationIntercom() calls set a nonzero pollIntervalMs with no ' +
    'createSseWatcher/spawnReader seam, so they construct a REAL SseWatcher and spawn a real ' +
    '"curl -sN" child process. Add `createSseWatcher: () => ({ close: () => {} })`' +
    ' (matching every other site) unless the test genuinely means to exercise real SSE:\n' +
    offenders.join('\n')
  expect(offenders, failureMessage).toEqual([])
})

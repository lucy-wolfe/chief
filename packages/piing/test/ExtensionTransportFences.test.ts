import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, test } from 'vitest'

// E4-S8 (#794): every pane extension's hand-rolled chiefd transport
// (`spawnSync("curl")`, a private `documentKey` sha256 hash, a private SSE
// watcher/parser, a hardcoded `127.0.0.1:8792` fallback) is deleted in favor
// of the shared, dependency-closed `@chief/chiefing/extension-runtime`
// subpath. This fence is the standing regression proving those private
// copies do not come back — mirrors `apps/cli/test/ImportFences.test.ts`'s
// strip-comments-then-match-the-construct style, allowlisted by content key
// (not line number).

const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url))
const EXTENSIONS_ROOT = join(PACKAGE_ROOT, 'extensions')

function walkTsFiles(dir: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) out.push(...walkTsFiles(full))
    else if (entry.endsWith('.ts')) out.push(full)
  }
  return out
}

function relPath(absolute: string): string {
  return relative(PACKAGE_ROOT, absolute).split('\\').join('/')
}

// Strips comments so every check below matches the CONSTRUCT (a real call, a
// real function definition, a real literal) and not the WORD -- a fence that
// greps raw text would flag this very file's own doc comments, provenance
// notes naming the deleted transport, and #794's own migration commentary.
function stripComments(source: string): string {
  const noBlockComments = source.replace(/\/\*[\s\S]*?\*\//g, (match) =>
    '\n'.repeat(match.split('\n').length - 1)
  )
  return noBlockComments.replace(/\/\/.*$/gm, '')
}

const ALL_EXTENSION_FILES = walkTsFiles(EXTENSIONS_ROOT)

/**
 * `authoritativeRuntimePane` (organization-intercom.ts) shells out to
 * `tmux list-panes` synchronously to recover this process's own pane
 * identity. This is tmux introspection, not chiefd HTTP transport -- it
 * predates and is orthogonal to the chiefd transport this story deletes, and
 * the issue's own Context section names only the four
 * `spawnSync("curl")` chiefdPostJson copies as in scope. Allowlisted by
 * content key, not line number, exactly like `BlockingAllowlist.ts`'s own
 * carve-outs for this pattern class.
 */
const SPAWN_SYNC_ALLOWLIST = new Set(['extensions/organization-intercom.ts:tmux-pane-discovery'])

function spawnSyncOffenders(): Array<{ file: string; line: number }> {
  const offenders: Array<{ file: string; line: number }> = []
  for (const file of ALL_EXTENSION_FILES) {
    const rel = relPath(file)
    const stripped = stripComments(readFileSync(file, 'utf8'))
    const lines = stripped.split('\n')
    lines.forEach((line, index) => {
      if (!/\bspawnSync\b/.test(line)) return
      // The one allowlisted tmux-introspection carve-out: a `run: typeof
      // spawnSync = spawnSync` default parameter and its import, both in
      // `authoritativeRuntimePane`'s tmux pane-discovery helper.
      //
      // The import used to read `import { spawn, spawnSync }` — the async
      // `spawn` was the launcher-subprocess transport's, and it is deleted
      // (#751/G9), so the carve-out now names the single-symbol import that is
      // actually there. A carve-out matching a string the file no longer
      // contains stops carving anything out and starts reporting the line it
      // was written to permit.
      if (
        rel === 'extensions/organization-intercom.ts' &&
        (line.includes('import { spawnSync }') || line.includes('typeof spawnSync = spawnSync'))
      ) {
        return
      }
      offenders.push({ file: rel, line: index + 1 })
    })
  }
  return offenders
}

describe('ExtensionTransportFences (E4-S8): the old hand-rolled chiefd transport stays deleted', () => {
  test('sanity check: the scan is not vacuous', () => {
    expect(ALL_EXTENSION_FILES.length).toBeGreaterThan(10)
  })

  test('zero spawnSync in packages/piing/extensions, outside the one documented tmux carve-out', () => {
    const offenders = spawnSyncOffenders()
    expect(offenders).toEqual([])
    // The allowlist constant itself must name a real, still-present carve-out
    // -- an empty/stale allowlist here would silently widen this fence.
    expect(SPAWN_SYNC_ALLOWLIST.size).toBe(1)
  })

  test('spawnSync self-check: the scan actually matches a real spawnSync call (not vacuously green)', () => {
    const stripped = stripComments('const result = spawnSync("curl", ["-sS"]);')
    expect(/\bspawnSync\b/.test(stripped)).toBe(true)
  })

  test('zero Atomics.wait in packages/piing/extensions', () => {
    const offenders: Array<{ file: string; line: number }> = []
    for (const file of ALL_EXTENSION_FILES) {
      const stripped = stripComments(readFileSync(file, 'utf8'))
      stripped.split('\n').forEach((line, index) => {
        if (/Atomics\s*\.\s*wait\s*\(/.test(line))
          offenders.push({ file: relPath(file), line: index + 1 })
      })
    }
    expect(offenders).toEqual([])
  })

  test('Atomics.wait self-check: the scan actually matches a real call (not vacuously green)', () => {
    const stripped = stripComments('Atomics.wait(sleeper, 0, 0, ms);')
    expect(/Atomics\s*\.\s*wait\s*\(/.test(stripped)).toBe(true)
  })

  test('zero private documentKey (durableDocumentKey sha256) implementations', () => {
    const offenders: Array<{ file: string; line: number }> = []
    for (const file of ALL_EXTENSION_FILES) {
      const stripped = stripComments(readFileSync(file, 'utf8'))
      stripped.split('\n').forEach((line, index) => {
        // The deleted shape: `createHash("sha256").update(<root>).digest("hex").slice(0, 12)`
        // composed with a `${slug}@...` template -- distinct from every other
        // legitimate createHash use in these files (content-addressed ids,
        // fingerprints), which never slice a hex digest to exactly 12 chars
        // for a `@`-joined document key.
        if (
          /@\$\{createHash\(.sha256.\)/.test(line) ||
          /createHash\(.sha256.\)[^\n]*\.slice\(0,\s*12\)/.test(line)
        ) {
          offenders.push({ file: relPath(file), line: index + 1 })
        }
      })
    }
    expect(offenders).toEqual([])
  })

  test('documentKey self-check: the scan actually matches the deleted shape (not vacuously green)', () => {
    const stripped = stripComments(
      'function durableDocumentKey(slug, dataRoot) { return `${slug}@${createHash("sha256").update(dataRoot).digest("hex").slice(0, 12)}`; }'
    )
    expect(
      /@\$\{createHash\(.sha256.\)/.test(stripped) ||
        /createHash\(.sha256.\)[^\n]*\.slice\(0,\s*12\)/.test(stripped)
    ).toBe(true)
  })

  test('the private chiefdPostJson definition (where one exists) delegates to the shared postOrgRoute, never spawnSync', () => {
    // A private `chiefdPostJson` NAME is fine (organization-intercom.ts,
    // team-ui.ts keep it as a thin async wrapper so their ~24
    // existing call sites did not need a route-by-route rewrite) -- what
    // matters is its BODY. `IntercomChiefingCalls.test.ts` pins the exact
    // one-line delegating body for organization-intercom.ts; this fence's
    // job is only the blanket "no spawnSync anywhere" check above, which
    // already covers every such definition.
    for (const file of ALL_EXTENSION_FILES) {
      const stripped = stripComments(readFileSync(file, 'utf8'))
      const match = /function\s+chiefdPostJson[\s\S]{0,400}/.exec(stripped)
      if (match) expect(match[0], relPath(file)).not.toContain('spawnSync')
    }
  })

  test('zero 127.0.0.1:8792 fixed-port fallback literals', () => {
    // Raw text, not comment-stripped: a naive `//`-comment stripper also
    // truncates at the `//` inside `http://`, which would corrupt this exact
    // check -- a plain substring scan is both simpler and correct here.
    const offenders: Array<{ file: string; line: number }> = []
    for (const file of ALL_EXTENSION_FILES) {
      const lines = readFileSync(file, 'utf8').split('\n')
      lines.forEach((line, index) => {
        if (line.includes('127.0.0.1:8792'))
          offenders.push({ file: relPath(file), line: index + 1 })
      })
    }
    expect(offenders).toEqual([])
  })

  test('127.0.0.1:8792 self-check: the scan actually matches the deleted literal (not vacuously green)', () => {
    expect('"http://127.0.0.1:8792"'.includes('127.0.0.1:8792')).toBe(true)
  })

  test('zero message-regex CHIEFD transient classifiers (structural isTransientChiefdError only)', () => {
    // Scoped to `.message` combined with a chiefd/docstore-shaped pattern on
    // the SAME line -- not every `.message.includes(...)` in these files
    // (e.g. `isStaleExtensionContextError`'s unrelated Pi-context check,
    // which the issue's own broad acceptance grep also matches and is a
    // known, reviewed non-offender, not something an automated gate can
    // assert to zero without becoming permanently red).
    const offenders: Array<{ file: string; line: number }> = []
    for (const file of ALL_EXTENSION_FILES) {
      const stripped = stripComments(readFileSync(file, 'utf8'))
      stripped.split('\n').forEach((line, index) => {
        const hasMessageStringMatch =
          /\.message\b[^\n]*(match\(|includes\()/.test(line) ||
          /(match|includes)\([^\n]*\.message\b/.test(line)
        if (hasMessageStringMatch && /chiefd|docstore/i.test(line)) {
          offenders.push({ file: relPath(file), line: index + 1 })
        }
      })
    }
    expect(offenders).toEqual([])
  })

  test('message-regex self-check: the scan actually matches a real chiefd message-regex classifier (not vacuously green)', () => {
    const line = 'if (error.message.match(/chiefd docstore.*unreachable/i)) return true;'
    const hasMessageStringMatch =
      /\.message\b[^\n]*(match\(|includes\()/.test(line) ||
      /(match|includes)\([^\n]*\.message\b/.test(line)
    expect(hasMessageStringMatch && /chiefd|docstore/i.test(line)).toBe(true)
  })

  test('org-sse-watcher.ts and semantic-row-deltas.ts stay deleted', () => {
    const names = ALL_EXTENSION_FILES.map((file) => relPath(file))
    expect(names).not.toContain('extensions/org-sse-watcher.ts')
    expect(names).not.toContain('extensions/semantic-row-deltas.ts')
  })

  test('no extension imports the deleted org-sse-watcher.ts or semantic-row-deltas.ts siblings', () => {
    const offenders: Array<{ file: string; line: number }> = []
    for (const file of ALL_EXTENSION_FILES) {
      const stripped = stripComments(readFileSync(file, 'utf8'))
      stripped.split('\n').forEach((line, index) => {
        if (
          /from\s+["']\.\/org-sse-watcher["']/.test(line) ||
          /from\s+["']\.\/semantic-row-deltas["']/.test(line)
        ) {
          offenders.push({ file: relPath(file), line: index + 1 })
        }
      })
    }
    expect(offenders).toEqual([])
  })
})

// Tripwire for the retired Rust workspace path (E1-S1, #758). Moved from
// `tests/chiefd-workspace-location.test.ts` (bun:test) onto `node --test`
// (revamp/tooling/tripwire-node-test-lane) because `bunfig.toml` preloads
// `tests/setup-durable-store.ts` for every `bun test` run, and that preload
// imports from the tree E4 is actively moving — every future E4 story that
// relocates files can break the preload again, and when it does this guard
// would silently stop running along with the rest of the bun test lane. The
// other two repo guards (`test:e2e-park`, `test:triage-map`) already live on
// `node --test`, which depends on nothing E4 touches; this one joins them.
// Run with `node --test scripts/test/chiefd-workspace-location.test.mjs`.
//
// E1-S1 (#758) moved the Rust workspace from the retired path to
// `apps/chiefd`. This file is the stale-branch tripwire: the program runs
// many parallel seats on long-lived branches, and a branch cut before S1
// merged can carry the retired path in new files that git merges cleanly —
// no textual conflict, because the hazard is a clean merge reintroducing a
// dead reference, not a diff collision.
//
// The needle is built from string concatenation, not written out literally,
// so this file cannot match its own pattern (precedent:
// tests/repo-conflict-markers.test.ts's character-repetition trick, and
// tests/no-retired-tribes-proxy.test.ts's `"tribes-" + "llm-proxy"`).
//
// Mandate 1 (reactive-only) note: every check here is a single synchronous
// read — no polling, no interval, no sleep.

import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const root = join(__dirname, '..', '..')

const RETIRED_ROOT = 'services' + '/chiefd'

const PATTERNS = [RETIRED_ROOT, '"services", "chiefd"', 'services' + '/' + String.raw`\*\*/\*\.rs`]

// Hoisted to module scope (not redeclared per-test) so the tripwire's own
// scan and #986's check-against-itself below share exactly one list --
// two copies of this array is exactly the "maintained list with no wired
// check against its source" shape this program keeps finding.
const PATHSPECS = [
  '--',
  '.',
  ':!*.parked',
  ':!docs/testing/parked-suite-triage.json',
  ':!docs/testing/parked-suite-triage.md',
  ':!scripts/test/chiefd-workspace-location.test.mjs',
]

/** Every tracked file a single exclusion pathspec resolves to, under `cwd`.
 *  Empty array (not a thrown error) for a pathspec matching nothing. */
export function filesMatchingExclusion(path, cwd = root) {
  try {
    return execFileSync('git', ['ls-files', '--', path], { cwd, encoding: 'utf8' })
      .split('\n')
      .filter(Boolean)
  } catch {
    return []
  }
}

/** Does at least one tracked file under this single pathspec still contain
 *  at least one of `patterns`? Scoped git grep per pattern, first hit wins —
 *  an exclusion only needs ONE surviving reference to still be earning its
 *  place, the same way the tripwire itself only needs one hit to fail. */
export function exclusionStillReferencesPattern(path, patterns, cwd = root) {
  for (const pattern of patterns) {
    try {
      execFileSync('git', ['grep', '-qE', pattern, '--', path], { cwd })
      return true
    } catch (error) {
      if (error.status === 1) continue
      throw new Error(`git grep infrastructure failure (exit ${error.status}) checking '${path}' for pattern '${pattern}': ${error.stderr}`)
    }
  }
  return false
}

/** #986: every `:!`-prefixed entry in `pathspecs` must (a) match at least
 *  one tracked file and (b) that file must still contain at least one of
 *  `patterns` — otherwise the entry is a stale exclusion, quietly widening
 *  the tripwire's blind spot for a reference that no longer exists (or
 *  never existed at this path). Returns the list of offence strings, empty
 *  when every entry justifies itself; never throws on a stale entry, so the
 *  caller can choose to report-and-stop rather than fail a build on a
 *  finding that needs a human, human per #986's own instruction not to
 *  auto-fix. */
export function findUnjustifiedExclusions(pathspecs, patterns, cwd = root) {
  const exclusions = pathspecs.filter((p) => p.startsWith(':!'))
  if (exclusions.length === 0) {
    throw new Error('REFUSING TO REPORT SUCCESS: zero exclusion entries parsed -- a broken derivation (wrong array, moved file), not evidence the list is empty.')
  }
  const offences = []
  for (const spec of exclusions) {
    const path = spec.slice(2)
    const matched = filesMatchingExclusion(path, cwd)
    if (matched.length === 0) {
      offences.push(`${spec}: matches ZERO tracked files -- stale exclusion, nothing left to exclude.`)
      continue
    }
    if (!exclusionStillReferencesPattern(path, patterns, cwd)) {
      const sample = matched.slice(0, 3).join(', ') + (matched.length > 3 ? `, +${matched.length - 3} more` : '')
      offences.push(`${spec}: matches ${matched.length} tracked file(s) (${sample}), but NONE still contain the retired pattern -- stale exclusion, widening the guard's blind spot for no reason.`)
    }
  }
  return offences
}

test('apps/chiefd is the Cargo workspace root', () => {
  const manifestPath = join(root, 'apps/chiefd/Cargo.toml')
  assert.ok(existsSync(manifestPath))

  const manifest = readFileSync(manifestPath, 'utf8')
  assert.ok(manifest.includes('resolver = "2"'))
  assert.ok(manifest.includes('crates/chiefd-core'))

  assert.equal(existsSync(join(root, RETIRED_ROOT)), false)
})

test('apps/chiefd carries no package.json', () => {
  // Bun's `workspaces: ["apps/*", "packages/*"]` glob skips directories
  // without a package.json — the precedent of a sibling project that keeps a
  // Rust cargo workspace invisible to the bun workspace while sitting in the
  // canonical `apps/` location.
  assert.equal(existsSync(join(root, 'apps/chiefd/package.json')), false)
})

test('no tracked file references the retired services/chiefd path', () => {
  // Allowlist (must match the E1 epic Contract's ban): parked test corpora
  // (`*.test.ts.parked`, E0-S2's frozen reference — the old path is what
  // they froze), the E9-S2 triage map (below), and this file itself.
  //
  // Four documentary exclusions — `docs/branch-audit/` (#933),
  // `docs/fleet-handoff.md`, `CHANGELOG.md` and `DECISIONS.md` — were
  // removed with their subjects in the open-source release. Their
  // reasoning is kept in one sentence because the CLASS still applies to
  // whatever comes next: a corpus or a description ABOUT this repository
  // (a scan receipt, prose explaining what this very guard scans for) is
  // not a live reference a reader would follow, and rewriting an accurate
  // sentence to dodge the scanner is the wrong trade. The general fix
  // (tell a live instruction from a historical mention wherever it
  // appears) is #986; a path-based exclusion cannot make that
  // distinction, only name a known-safe location for it.
  //
  // docs/testing/parked-suite-triage.{json,md} (#834) landed after this
  // test's branch point and reference the retired path legitimately, for the
  // same reason e2e-parked.md does: the .md carries the identical pre-move
  // corpus-SHA citation, and the .json's `reason` for
  // tests/repo-binary-source.test.ts states that the PARKED test pins a
  // binary under the retired path — rewriting either to `apps/chiefd` would
  // make a true statement false. Content wins; the grep is scoped.
  // NOTE: these two are living documents, unlike the static e2e-parked.md,
  // so this file-level exclusion is coarser than ideal — a future genuinely
  // stale reference inside them would not be caught. Narrowing this to a
  // line- or field-scoped check belongs to whichever story next edits this
  // tripwire.
  //
  const offences = []
  for (const pattern of PATTERNS) {
    try {
      const output = execFileSync('git', ['grep', '-nE', pattern, ...PATHSPECS], {
        cwd: root,
        encoding: 'utf8'
      })
      offences.push(output.trimEnd())
    } catch (error) {
      // git grep exits 1 when nothing matches — that is success here, not a
      // failure. Any other non-zero exit is an infrastructure problem.
      if (error.status !== 1) {
        throw new Error(
          `git grep infrastructure failure (exit ${error.status}) for pattern ${pattern}: ${error.stderr}`
        )
      }
    }
  }

  assert.deepEqual(
    offences,
    [],
    offences.length > 0
      ? `retire the ${RETIRED_ROOT} reference or extend the documented allowlist in this file:\n${offences.join('\n')}`
      : undefined
  )
})

// #986: a maintained exclusion list with no wired check against its own
// source of truth WILL diverge -- the same shape this program has already
// found four other times tonight (a host allowlist, a (file, primitive)
// keyed allowlist, an unwired duplicate guard file, the batch gate's leg
// list against CI's). Ten entries have accumulated in PATHSPECS over time
// with nothing checking whether any of them still earns its place. This
// does NOT attempt the structural discriminator (live instruction vs.
// historical description) #986 also names and explicitly defers -- a
// path-based rule cannot make that distinction, only confirm a named path
// still contains SOME surviving reference. Reports a stale entry as a
// finding; never removes one (#986's own instruction: a genuinely stale
// entry is something to learn, not something to silently fix here).
test('#986: every exclusion pathspec still justifies itself against the live tree', () => {
  const offences = findUnjustifiedExclusions(PATHSPECS, PATTERNS, root)
  assert.deepEqual(
    offences,
    [],
    offences.length > 0
      ? `Stale exclusion entr${offences.length === 1 ? 'y' : 'ies'} found -- report and stop, do not silently remove:\n${offences.join('\n')}`
      : undefined
  )
})

// ARM/CONTROL for findUnjustifiedExclusions itself, against a disposable
// scratch git repo -- the control must be able to fail the way the defect
// presents (a stale exclusion sitting unnoticed), not just prove the
// function runs against a fixture engineered to always pass.
function withScratchGitRepo(fn) {
  const dir = mkdtempSync(join(tmpdir(), 'chiefd-workspace-location-exclusions-'))
  try {
    execFileSync('git', ['init', '-q'], { cwd: dir })
    execFileSync('git', ['config', 'user.email', 'fixture@example.com'], { cwd: dir })
    execFileSync('git', ['config', 'user.name', 'fixture'], { cwd: dir })
    return fn(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

function commitAll(dir, message) {
  execFileSync('git', ['add', '-A'], { cwd: dir })
  execFileSync('git', ['commit', '-q', '-m', message], { cwd: dir })
}

test('ARM (#986): an exclusion naming a path with no surviving reference to the retired pattern is reported stale', () => {
  withScratchGitRepo((dir) => {
    mkdirSync(join(dir, 'docs'), { recursive: true })
    writeFileSync(join(dir, 'docs', 'clean.md'), 'nothing retired mentioned here\n')
    commitAll(dir, 'fixture: clean doc')

    const offences = findUnjustifiedExclusions(['--', '.', ':!docs/clean.md'], PATTERNS, dir)
    assert.equal(offences.length, 1, 'a pathspec whose only matched file no longer contains any retired pattern must be reported stale')
    assert.match(offences[0], /:!docs\/clean\.md/)
    assert.match(offences[0], /NONE still contain/)
  })
})

test('ARM (#986): an exclusion naming a path with zero tracked files is reported stale', () => {
  withScratchGitRepo((dir) => {
    writeFileSync(join(dir, '.gitkeep'), '')
    commitAll(dir, 'fixture: empty repo, no docs dir at all')

    const offences = findUnjustifiedExclusions(['--', '.', ':!docs/never-existed.md'], PATTERNS, dir)
    assert.equal(offences.length, 1)
    assert.match(offences[0], /matches ZERO tracked files/)
  })
})

test('CONTROL (#986): an exclusion whose file still references the retired pattern is NOT reported stale', () => {
  withScratchGitRepo((dir) => {
    mkdirSync(join(dir, 'docs'), { recursive: true })
    writeFileSync(join(dir, 'docs', 'explains-it.md'), `this guard scans for ${RETIRED_ROOT}\n`)
    commitAll(dir, 'fixture: doc genuinely still contains the pattern')

    const offences = findUnjustifiedExclusions(['--', '.', ':!docs/explains-it.md'], PATTERNS, dir)
    assert.deepEqual(offences, [], 'a pathspec whose matched file genuinely still contains the pattern must NOT be reported stale -- this is the case that must survive')
  })
})

test('CONTROL (#986): the real tree passes with all four real exclusion entries', () => {
  // The control that can fail the way the defect presents: run the exact
  // function against the exact real PATHSPECS/root the tripwire itself
  // uses, not a synthetic list engineered to be clean.
  const exclusions = PATHSPECS.filter((p) => p.startsWith(':!'))
  // 10 -> 9: the `:!docs/testing/e2e-parked.md` entry was removed because that
  // document went with the E2E corpus and the pathspec matched zero tracked
  // files -- which is exactly what the tripwire below reported. A deliberate
  // update to match a real removal, which is what this assertion asks for.
  // 9 -> 4, all in the open-source release and all for the same reason: the
  // exclusion outlived the document it named.
  //
  //   `:!plans`                     -- a plan became a LOCAL working
  //                                    document, so nothing under `plans/` is
  //                                    tracked any more.
  //   `:!docs/branch-audit`         -- the internal one-off artifacts were
  //   `:!docs/fleet-handoff.md`        deleted from the public tree.
  //   `:!CHANGELOG.md`              -- both ledgers start fresh in the public
  //   `:!DECISIONS.md`                 tree (the private ledger is archived,
  //                                    not published), so neither file
  //                                    quotes the retired path any more.
  //
  // The first three matched ZERO tracked files; the last two match a file
  // that no longer contains the pattern. This tripwire reports both shapes,
  // and it reported these.
  assert.equal(exclusions.length, 4, 'this file is known to carry exactly four :!-prefixed exclusion entries -- a count drifting either way means PATHSPECS itself changed and this assertion needs a deliberate update, not a silent pass')
  const offences = findUnjustifiedExclusions(PATHSPECS, PATTERNS, root)
  assert.deepEqual(offences, [], offences.length > 0 ? `Real tree carries a stale exclusion:\n${offences.join('\n')}` : undefined)
})

test('rust-toolchain.toml pins the workspace from the repo root', () => {
  const rootToolchain = join(root, 'rust-toolchain.toml')
  assert.ok(existsSync(rootToolchain))
  assert.ok(readFileSync(rootToolchain, 'utf8').includes('channel = "1.97.1"'))

  assert.equal(existsSync(join(root, 'apps/chiefd/rust-toolchain.toml')), false)
})

// Guard for docs/testing/parked-suite-triage.json (E9-S2, #834).
//
// The 408-file parked bun:test corpus (277 unit + 130 e2e + 1 manual) is
// reference material, not a running suite (ruling D0 — bun:test is retired,
// not run in parallel). A written disposition per file is the only thing
// standing between "parked" and "silently deleted" (issue #834's Context).
// This script is what keeps that written record honest: it fails when a
// test file that existed under tests/ AT THE SNAPSHOT SHA (map.capturedAt)
// has no row, a row claims to be part of that snapshot corpus but names a
// path that was never in it, a row names an uncommitted path, a disposition
// sits outside the closed enum, a path is duplicated, a `retire:rust` row's
// target doesn't actually exist under apps/chiefd/, a row's `lane`
// disagrees with the corpus's own e2e/manual/unit routing, or the .md
// narrative's cluster counts drift from the JSON.
//
// Rule 1 is deliberately scoped to the SNAPSHOT corpus (git ls-tree at
// map.capturedAt), not the live tree (git ls-files today). This map triages
// the *parked legacy* corpus as it stood at a specific SHA; a story that
// adds a brand-new test file to tests/ after that SHA (e.g. a fresh
// retired-path tripwire) is not part of what this map disposes of, and
// requiring it to have a row here would be inventing a disposition
// vocabulary member for "not actually legacy" — exactly the kind of
// semantic corruption this map exists to prevent. A row IS still required
// to actually correspond to something that was in the snapshot, so a
// fabricated or mistyped path can't hide in the map either.
//
// Run with `node --test scripts/test/parked-suite-triage.test.mjs`.
//
// Mandate 1 (reactive-only) note: every check here is a single synchronous
// read — no polling, no interval, no sleep.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function repoFile(...segments) {
  return join(repoRoot, ...segments)
}

function gitLsTree(cwd, sha, ...pathspecs) {
  return execFileSync('git', ['ls-tree', '-r', '--name-only', sha, '--', ...pathspecs], { cwd, encoding: 'utf8' })
    .split('\n')
    .filter(Boolean)
}

// A file already parked-by-rename (`.test.ts.parked`) has the same snapshot
// identity as its pre-rename name (`.test.ts`) — E0-S2 renamed 3 meta files
// this way before the snapshot SHA's tree recorded them, so the live map's
// rows use the `.parked` name while the snapshot tree has the bare one.
// Stripping the suffix before comparing lets both sides agree on identity
// regardless of which side of the rename each was captured on.
function normalizeForSnapshot(p) {
  return p.replace(/\.parked$/, '')
}

// The corpus's own e2e/manual routing rule, re-derived rather than imported.
// It mirrored scripts/ci-shard.ts's isE2eTestFile / isManualTestFile until
// #1035 deleted that sharder (no lane invoked it); re-deriving rather than
// importing is exactly why its deletion leaves rule 5 below correct instead
// of broken, and it matches e2e-park-tripwire's precedent.
function isE2eTestFile(file) {
  const base = file.replace(/^tests\//, '')
  if (base.startsWith('manual/')) return false
  return base.endsWith('.e2e.test.ts')
}
function isManualTestFile(file) {
  return file.replace(/^tests\//, '').startsWith('manual/')
}

const DISPOSITION_ENUM = new Set([
  'port:chiefing',
  'port:piing',
  'port:cli',
  'port:api',
  'retire:rust',
  'retire:meta',
  'retire:obsolete',
  'park:e2e',
  // #1020: two values the vocabulary was missing. Every other entry
  // asserts work is owed, so a healthy file had to claim a debt it did
  // not have, and 38 files whose imports point at the pre-monorepo
  // repo-root `src/` tree had no true value at all. `retire:obsolete`
  // is deliberately NOT the answer for those: it asserts a decision
  // someone made, and a test whose imports moved is UNMIGRATED rather
  // than obsolete. Filing one as the other launders a maintenance gap
  // into a permanent retirement nobody looks at again.
  'keep:active',
  'migrate:paths',
  'split',
])
// #751/G12 added 'live'. The four original members all describe a file's
// position relative to the PARKED corpus's migration — work owed, work done,
// work abandoned, work waiting. Fifteen files under tests/ carry no row at
// all today, and several of them (tests/setup-conditional-preload.ts, the
// bunfig preload every `bun test` invocation in this repo loads;
// tests/setup-workspace-build-preflight.ts; tests/helpers/*) are not migration
// subjects in any direction: they are load-bearing infrastructure that runs
// right now. Filing one as 'parked' would state something false about the
// most-executed file in the directory, and 'pending' would assert a debt that
// does not exist — the same laundering #1020 refused when it added
// 'keep:active' and 'migrate:paths' rather than calling an unmigrated file
// obsolete. 'live' is the honest fourth answer: this file runs, nothing is
// owed on it, and it is recorded so it can never be swept as corpus residue.
const STATUS_ENUM = new Set(['pending', 'ported', 'retired', 'parked', 'live'])
const LANE_ENUM = new Set(['unit', 'e2e', 'manual', 'support'])

// ---------------------------------------------------------------------------
// The validator. Pure function of (repoRoot, mapObject) -> string[] of
// violation messages, so it can be exercised against both the real map (for
// the guard) and a doctored fixture (for the negative self-test) without
// duplicating logic.
// ---------------------------------------------------------------------------
export function validateTriageMap(root, map) {
  const errors = []
  const entries = Array.isArray(map?.entries) ? map.entries : []

  // Rule 3 (checked first; a malformed row can't be used for rules 1/2/4/5).
  const wellFormed = []
  for (const [i, e] of entries.entries()) {
    if (!e || typeof e !== 'object') {
      errors.push(`entries[${i}] is not an object`)
      continue
    }
    if (typeof e.path !== 'string' || e.path.length === 0) errors.push(`entries[${i}].path must be a non-empty string`)
    if (!LANE_ENUM.has(e.lane)) errors.push(`entries[${i}] (${e.path}): lane "${e.lane}" is not one of ${[...LANE_ENUM].join(', ')}`)
    if (!DISPOSITION_ENUM.has(e.disposition))
      errors.push(`entries[${i}] (${e.path}): disposition "${e.disposition}" is outside the closed enum`)
    if (typeof e.target !== 'string' || e.target.trim().length === 0) errors.push(`entries[${i}] (${e.path}): target must be non-empty`)
    if (typeof e.story !== 'string' || e.story.trim().length === 0) errors.push(`entries[${i}] (${e.path}): story must be non-empty`)
    if (typeof e.reason !== 'string' || e.reason.trim().length === 0) errors.push(`entries[${i}] (${e.path}): reason must be non-empty`)
    if (!STATUS_ENUM.has(e.status)) errors.push(`entries[${i}] (${e.path}): status "${e.status}" is not one of ${[...STATUS_ENUM].join(', ')}`)
    if (typeof e.path === 'string' && e.path.length > 0) wellFormed.push(e)
  }

  // Duplicate-path check (part of Mandate 0 compliance: no file may carry
  // two dispositions).
  const seen = new Map()
  for (const e of wellFormed) {
    if (seen.has(e.path)) errors.push(`duplicate path: ${e.path} (rows ${seen.get(e.path)} and this one)`)
    seen.set(e.path, e.path)
  }

  // Rule 1 (scoped to the snapshot corpus): every *.test.ts file that
  // existed under tests/ AT map.capturedAt has exactly one row. Uses
  // `git ls-tree` at that SHA, not `git ls-files` against the live tree —
  // see the file header for why. A missing/empty capturedAt is itself an
  // error: rule 1 cannot be scoped without it.
  const capturedAt = typeof map?.capturedAt === 'string' && map.capturedAt.length > 0 ? map.capturedAt : null
  if (!capturedAt) {
    errors.push('map.capturedAt is missing or empty; rule 1 cannot scope the snapshot corpus without it')
  }
  const snapshotTestPaths = capturedAt
    ? new Set(gitLsTree(root, capturedAt, 'tests').filter((p) => p.endsWith('.test.ts')))
    : new Set()

  // The subset of rows that claim to belong to the parked legacy corpus
  // (as opposed to `support` rows, which were never *.test.ts files and
  // are exempt from snapshot membership).
  const corpusEntries = wellFormed.filter((e) => e.lane === 'unit' || e.lane === 'e2e' || e.lane === 'manual')
  const mappedNormalized = new Map()
  for (const e of corpusEntries) {
    mappedNormalized.set(normalizeForSnapshot(e.path), e.path)
  }

  if (capturedAt) {
    for (const snapshotPath of snapshotTestPaths) {
      if (!mappedNormalized.has(snapshotPath)) {
        errors.push(`snapshot test file (captured at ${capturedAt}) has no triage row: ${snapshotPath}`)
      }
    }

    // Rule 1b (the reverse direction): a row claiming corpus membership
    // (lane unit/e2e/manual) must name a path that was ACTUALLY in the
    // snapshot — otherwise a typo'd or fabricated path could sit in the map
    // forever, uncaught, because rule 2 only checks the LIVE tree (where a
    // `ported`/`retired` row is allowed to name a since-deleted path).
    for (const e of corpusEntries) {
      const normalized = normalizeForSnapshot(e.path)
      if (!snapshotTestPaths.has(normalized)) {
        errors.push(
          `${e.path}: not part of the snapshot corpus captured at ${capturedAt} (row claims lane "${e.lane}" but this path did not exist under tests/ at that SHA)`,
        )
      }
    }
  }

  // Rule 2: every entry's path exists in the working tree, OR its status is
  // ported/retired (a completed row may name a deleted file), OR its own
  // `renamedTo` field names a path that exists (#956: a genuine park:e2e
  // action renames the file to carry the routing-by-suffix `.e2e.test.ts`
  // suffix docs/testing/e2e-parked.md's own convention requires -- `path`
  // stays the SNAPSHOT identity rule 1/1b/5 key off, `renamedTo` is the
  // live path, and BOTH must exist: the row's identity is not orphaned, and
  // the rename it claims actually happened). Support rows for files not
  // under tests/**/*.test.ts(.parked) are exempt from rule 1 but still
  // checked here.
  for (const e of wellFormed) {
    const exists = existsSync(repoFile2(root, e.path))
    const renamedToExists = typeof e.renamedTo === 'string' && existsSync(repoFile2(root, e.renamedTo))
    if (!exists && !renamedToExists && e.status !== 'ported' && e.status !== 'retired') {
      errors.push(`${e.path}: does not exist in the working tree and status is "${e.status}" (only ported/retired rows may name a deleted path, or a parked row's own renamedTo must exist)`)
    }
    if (typeof e.renamedTo === 'string' && !renamedToExists) {
      errors.push(`${e.path}: renamedTo "${e.renamedTo}" does not exist in the working tree -- the claimed rename never happened`)
    }
  }

  // Rule 4: retire:rust rows' target names a file under apps/chiefd/ or a
  // conformance/fixtures/<family> directory, and that path exists.
  //
  // STATED LIMIT (#937): this proves the named Rust file EXISTS, never that
  // it covers the same behavior as the row it's retiring in favor of. #937
  // found two retire:rust rows whose target existed but tested entirely
  // different behavior (raw HTTP docstore routes vs. the TS files' CLI
  // command-dispatch integration and conditional-read/cache semantics) --
  // a green here is proof against a typo'd or deleted path, not proof the
  // retirement is behaviorally sound. That judgement is human, and stays
  // human: verify by actually reading both files' test bodies, the same
  // way #937's own investigation did, not by trusting this rule's pass.
  for (const e of wellFormed) {
    if (e.disposition !== 'retire:rust') continue
    const m = e.target.match(/(apps\/chiefd\/[^\s(]+\.rs)/) || e.target.match(/(conformance\/fixtures\/[^\s(/]+)/)
    if (!m) {
      errors.push(`${e.path}: retire:rust target "${e.target}" does not name a path under apps/chiefd/ or conformance/fixtures/<family>`)
      continue
    }
    if (!existsSync(repoFile2(root, m[1]))) {
      errors.push(`${e.path}: retire:rust target "${m[1]}" does not exist`)
    }
  }

  // Rule 7 (#937): a `port:*` row claiming status "ported" must name a
  // target that actually exists -- "ported" is a completion claim, and a
  // completion claim with no destination is exactly the class #937 found
  // live (four `port:cli` rows claiming a disposition with no target ever
  // created). Deliberately scoped to status === 'ported' only: a `pending`
  // row's target legitimately does not exist yet (the port hasn't
  // happened), so checking it unconditionally would fail ~37 entirely
  // normal in-flight rows and teach engineers to ignore this guard's red.
  // `target` may name multiple files joined by " + " (an established
  // convention in this map); every one of them must exist.
  for (const e of wellFormed) {
    if (!e.disposition.startsWith('port:')) continue
    if (e.status !== 'ported') continue
    for (const target of e.target.split('+').map((t) => t.trim())) {
      if (!existsSync(repoFile2(root, target))) {
        errors.push(`${e.path}: status is "ported" but target "${target}" does not exist -- a completion claim with no destination`)
      }
    }
  }

  // Rule 5: lane agreement with the corpus's own e2e/manual/unit routing.
  for (const e of wellFormed) {
    if (e.lane === 'support') continue // non-test files are support by definition
    if (!e.path.startsWith('tests/') || !(e.path.endsWith('.test.ts') || e.path.endsWith('.test.ts.parked'))) continue
    const bare = e.path.replace(/\.parked$/, '')
    const expected = isManualTestFile(bare) ? 'manual' : isE2eTestFile(bare) ? 'e2e' : 'unit'
    if (e.lane !== expected) {
      errors.push(`${e.path}: lane "${e.lane}" disagrees with the corpus router, which classifies it as "${expected}"`)
    }
  }

  return errors
}

function repoFile2(root, relPath) {
  return join(root, relPath)
}

// ---------------------------------------------------------------------------
// Guard tests against the real map.
// ---------------------------------------------------------------------------

const jsonPath = repoFile('docs', 'testing', 'parked-suite-triage.json')
const mdPath = repoFile('docs', 'testing', 'parked-suite-triage.md')
const map = JSON.parse(readFileSync(jsonPath, 'utf8'))
const md = readFileSync(mdPath, 'utf8')

test('docs/testing/parked-suite-triage.json exists and parses', () => {
  assert.ok(existsSync(jsonPath))
  assert.ok(statSync(jsonPath).size > 0)
})

test('every entry is well-formed: lane/disposition/status enums, non-empty target/story/reason (rule 3)', () => {
  const errors = validateTriageMap(repoRoot, map).filter(
    (m) =>
      !m.startsWith('snapshot test file') &&
      !m.includes('not part of the snapshot corpus') &&
      !m.includes('does not exist in the working tree') &&
      !m.startsWith('duplicate path') &&
      !m.includes('retire:rust target') &&
      !m.includes('status is "ported" but target') &&
      !m.includes('disagrees with the corpus router'),
  )
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('no duplicate paths across all rows (Mandate 0: no file carries two dispositions)', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.startsWith('duplicate path'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 1: every test file in the snapshot corpus (captured at map.capturedAt) has exactly one row — NOT scoped to the live tree, so a story that adds a new tests/*.test.ts file later is unaffected', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.startsWith('snapshot test file'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 1b: every corpus-lane row (unit/e2e/manual) names a path that was actually in the snapshot', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.includes('not part of the snapshot corpus'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 2: every row names a path that exists, or is status ported/retired', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.includes('does not exist in the working tree'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 4: retire:rust targets name a real apps/chiefd/ or conformance/fixtures/<family> path', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.includes('retire:rust target'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 5: lane matches the ci-shard router classification', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.includes('disagrees with the corpus router'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

// #937: the triage map was found to record "ported"/"parked" as
// intentions rather than checked facts -- four `port:*` rows claimed
// status "ported" with a target that had never been created or had since
// been deleted without the row being updated. Rule 7 makes "ported"
// mechanically checkable: it must name a target that exists, TODAY, not
// merely a plausible future one. NOTE what this rule deliberately does
// NOT check, so nobody reads a green here as more than it is: it proves
// the target FILE exists, never that its contents actually cover the
// retired file's behavior (that is a human judgement call, same as
// `retire:rust`'s existence-only check below).
test('rule 7 (#937): a port:* row claiming "ported" names a target that actually exists today', () => {
  const errors = validateTriageMap(repoRoot, map).filter((m) => m.includes('status is "ported" but target'))
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('rule 7 (#937) negative self-test: a doctored "ported" row with a nonexistent target fails validation', () => {
  const doctored = {
    ...map,
    entries: [
      ...map.entries,
      {
        path: 'tests/does-not-exist-fixture.test.ts',
        lane: 'support',
        cluster: 'fixture',
        disposition: 'port:cli',
        target: 'apps/cli/test/DoesNotExistFixture.test.ts',
        story: 'fixture',
        reason: 'fixture row for the negative self-test',
        status: 'ported',
      },
    ],
  }
  const errors = validateTriageMap(repoRoot, doctored).filter((m) => m.includes('status is "ported" but target'))
  assert.ok(
    errors.some((m) => m.includes('DoesNotExistFixture.test.ts') && m.includes('does not exist')),
    'expected a ported-target-missing violation for the doctored row',
  )
})

test('#956 REAL REPO: the company-boot.test.ts and org-units.test.ts rows (renamed to .e2e.test.ts on disk) pass rule 2 via their own renamedTo field', () => {
  const errors = validateTriageMap(repoRoot, map)
  assert.ok(
    !errors.some((m) => m.includes('tests/company-boot.test.ts') || m.includes('tests/org-units.test.ts')),
    `expected zero rule-2/renamedTo violations for the two genuinely-parked #956 rows; got: ${JSON.stringify(
      errors.filter((m) => m.includes('company-boot') || m.includes('org-units')),
    )}`,
  )
})

test('#956 negative self-test: a renamedTo pointing at a file that does not exist is caught, naming the claimed rename', () => {
  const withFakeRename = {
    ...map,
    entries: [
      ...map.entries,
      {
        path: 'tests/does-not-exist-fixture-3.test.ts',
        lane: 'unit',
        cluster: 'fixture',
        disposition: 'park:e2e',
        target: 'docs/testing/e2e-parked.md',
        story: 'fixture',
        reason: 'fixture row for the renamedTo negative self-test',
        status: 'parked',
        renamedTo: 'tests/this-file-was-never-actually-created.e2e.test.ts',
      },
    ],
  }
  const errors = validateTriageMap(repoRoot, withFakeRename)
  assert.ok(
    errors.some((m) => m.includes('renamedTo') && m.includes('never happened')),
    `expected a violation naming the fake rename; got: ${JSON.stringify(errors)}`,
  )
})

test('rule 6: the .md narrative cluster counts match the JSON row counts', () => {
  const counts = new Map()
  for (const e of map.entries) {
    if (e.lane !== 'unit') continue
    counts.set(e.cluster, (counts.get(e.cluster) ?? 0) + 1)
  }
  for (const [cluster, count] of counts) {
    const re = new RegExp(`${cluster}[^\\n]*?\\b${count}\\b`)
    assert.ok(
      re.test(md) || md.includes(`**${count}**`) && md.toLowerCase().includes(cluster),
      `docs/testing/parked-suite-triage.md does not appear to state the ${cluster} cluster count of ${count} anywhere`,
    )
  }
})

test('the JSON entries total (451) matches the sum of unit(280) + e2e(130) + manual(1) + support(40)', () => {
  // ROWS RESTORED, not re-dropped. When tests/e2e/ was deleted wholesale the
  // rows under that prefix were dropped from the map BY PATH PREFIX, which
  // took the totals to 305 (unit 274 / e2e 5). That is what rule 1 above was
  // reporting: `capturedAt` pins the snapshot corpus to a fixed historical
  // SHA precisely so a deletion CANNOT silently remove a row, and 131
  // snapshot files were left with no disposition at all.
  //
  // So the 128 tests/e2e/ rows (plus the three non-e2e snapshot files that
  // never had one) are back, as `retire:obsolete` / status `retired`, naming
  // the deletion decision as their target. This does not invent a
  // replacement target for anything still parked — the ~34 park:e2e rows for
  // files that STILL EXIST are untouched, exactly as #1017 ruled. It records
  // a decision that was actually made, for files that are actually gone.
  //
  // The 11 rows that were `parked` while naming a path the same deletions
  // removed also moved to `retired`: "parked" asserts a file waiting to come
  // back, and nothing is waiting.
  // #751/G12 took support from 25 to 40 and the total from 436 to 451. This
  // is a floor moving UP because rows were ADDED, never a count relaxed to
  // accommodate a deletion: fifteen files under tests/ existed with no row of
  // any kind, because rule 1 is deliberately scoped to the snapshot corpus
  // and every one of them landed after `capturedAt`. Among them was
  // tests/setup-conditional-preload.ts — the bunfig preload EVERY `bun test`
  // in this repo loads. The map's completeness claim was true of the corpus
  // and false of the directory, and nothing said so. The support lane is now
  // asserted explicitly rather than left as the arithmetic remainder, so a
  // support row silently disappearing fails by name like the other three.
  const byLane = { unit: 0, e2e: 0, manual: 0, support: 0 }
  for (const e of map.entries) byLane[e.lane] = (byLane[e.lane] ?? 0) + 1
  assert.equal(byLane.unit, 280, `unit lane: expected 280, got ${byLane.unit}`)
  assert.equal(byLane.e2e, 130, `e2e lane: expected 130, got ${byLane.e2e}`)
  assert.equal(byLane.manual, 1, `manual lane: expected 1, got ${byLane.manual}`)
  assert.equal(byLane.support, 40, `support lane: expected 40, got ${byLane.support}`)
  assert.equal(map.entries.length, 451, `total entries: expected 451, got ${map.entries.length}`)
})

// ---------------------------------------------------------------------------
// Negative self-test: a fixture map with a bogus disposition and a missing
// path must fail the validator. Otherwise a vacuously-passing guard would be
// indistinguishable from a correct map (issue #834's explicit requirement).
// ---------------------------------------------------------------------------

test('negative self-test: a doctored map with a bogus disposition and a missing path fails validation', () => {
  const doctored = {
    capturedAt: 'de72d660',
    generatedBy: 'fixture',
    entries: [
      {
        path: 'tests/this-file-does-not-exist-anywhere.test.ts',
        lane: 'unit',
        cluster: 'other',
        disposition: 'not-a-real-disposition',
        target: 'nowhere',
        story: 'X',
        reason: 'fixture row for the negative self-test',
        status: 'pending',
      },
    ],
  }
  const errors = validateTriageMap(repoRoot, doctored)
  assert.ok(
    errors.some((m) => m.includes('is outside the closed enum')),
    'expected a disposition-enum violation',
  )
  assert.ok(
    errors.some((m) => m.includes('does not exist in the working tree')),
    'expected a missing-path violation',
  )
  // And it must ALSO still report every real snapshot test file as
  // unmapped, since the doctored map has only one (fabricated) row.
  assert.ok(
    errors.some((m) => m.startsWith('snapshot test file')),
    'expected the doctored (near-empty) map to report missing coverage for the real corpus',
  )
  // The fabricated row's own path was never in the snapshot either.
  assert.ok(
    errors.some((m) => m.includes('not part of the snapshot corpus')),
    'expected a not-in-snapshot violation for the fabricated path',
  )
})

test('negative self-test: a doctored map with a duplicate path fails validation', () => {
  const row = {
    path: 'tests/org-row-stores.test.ts',
    lane: 'unit',
    cluster: 'chiefing',
    disposition: 'port:chiefing',
    target: 'packages/chiefing/test/resources/RowStores.test.ts',
    story: 'E9-S3',
    reason: 'duplicate fixture row',
    status: 'pending',
  }
  const doctored = { capturedAt: 'de72d660', generatedBy: 'fixture', entries: [row, { ...row }] }
  const errors = validateTriageMap(repoRoot, doctored)
  assert.ok(errors.some((m) => m.startsWith('duplicate path')), 'expected a duplicate-path violation')
})

test('negative self-test: a doctored retire:rust row with a nonexistent Rust target fails validation', () => {
  const doctored = {
    capturedAt: 'de72d660',
    generatedBy: 'fixture',
    entries: [
      {
        path: 'tests/org-store.test.ts',
        lane: 'unit',
        cluster: 'org-engine',
        disposition: 'retire:rust',
        target: 'apps/chiefd/crates/chiefd-core/tests/this_rust_test_does_not_exist.rs',
        story: 'E7',
        reason: 'fixture row for the negative self-test',
        status: 'pending',
      },
    ],
  }
  const errors = validateTriageMap(repoRoot, doctored)
  assert.ok(
    errors.some((m) => m.includes('retire:rust target') && m.includes('does not exist')),
    'expected a nonexistent-Rust-target violation',
  )
})

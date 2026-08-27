// The shape guard for `.github/workflows/ci.yml` (E9-S6, #751/G10).
//
// E9-S6 asked for a workflow-shape check — job set, banner text, no
// `continue-on-error` — and it was never implemented. The only ci.yml-shape
// assertions in the tree lived in `tests/ci-workflow.test.ts`, inside the
// parked `bun test tests` corpus, so they ran in NO lane: the triage map's
// own row for that file says as much ("it is unrun only because the whole
// tests/ corpus is parked"). Its disposition was `keep:active`, and this file
// is where it was kept: every assertion it made is below, plus the ones E9-S6
// asked for that never existed.
//
// The gap those missing assertions left, stated plainly, because it is the
// reason this file is not optional: at the time it was written this workflow
// carried a banner reading "All CI test execution is deliberately disabled"
// directly above THREE live test jobs, and a job titled "Repo guards
// (seventeen node --test invariants)" running forty-two of them. Both were
// true when written and neither was ever revisited, because prose in a
// workflow is checked by nobody. A CI file that misdescribes itself is worse
// than an undocumented one: every reader downstream reasons from it.
//
// Run with `node --test scripts/test/ci-workflow-shape.test.mjs`.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { GUARD_WIRING_MANIFEST } from '../guard-wiring-manifest.mjs'
import { deriveExpectedCounts } from '../cargo-test-derive.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const ciPath = join(repoRoot, '.github', 'workflows', 'ci.yml')

function readCi() {
  return readFileSync(ciPath, 'utf8')
}

// Top-level job names: the two-space-indented keys under `jobs:`. Derived
// from the file rather than transcribed, so a renamed or deleted job shows up
// as a set difference naming itself.
export function jobNames(workflow) {
  const lines = workflow.split('\n')
  const start = lines.findIndex((line) => /^jobs:\s*$/.test(line))
  if (start < 0) return []
  const names = []
  for (const line of lines.slice(start + 1)) {
    if (/^\S/.test(line) && line.trim().length > 0) break
    const match = /^ {2}([A-Za-z0-9][\w-]*):\s*$/.exec(line)
    if (match) names.push(match[1])
  }
  return names
}

// Lines that are inside a `run:` value rather than a comment or a key. Same
// reason workflow-script-resolution.test.mjs draws this line: ci.yml's prose
// legitimately quotes commands (including retired ones), and a guard that
// cannot tell a citation from an invocation makes an honest comment a build
// failure.
export function runStepLines(workflow) {
  const lines = workflow.split('\n')
  const out = []
  let blockIndent
  const indentOf = (line) => line.length - line.replace(/^\s*/, '').length
  for (const raw of lines) {
    const trimmed = raw.trim()
    if (blockIndent !== undefined) {
      if (trimmed.length === 0 || indentOf(raw) > blockIndent) {
        if (!trimmed.startsWith('#')) out.push(trimmed)
        continue
      }
      blockIndent = undefined
    }
    const match = /^(?:-\s*)?run:\s*(.*)$/.exec(trimmed)
    if (!match) continue
    const value = match[1].trim()
    if (value === '|' || value === '>' || value === '|-' || value === '>-') {
      blockIndent = indentOf(raw)
      continue
    }
    out.push(value)
  }
  return out
}

// Does this workflow actually execute tests? Derived from its `run:` steps,
// never assumed — the banner check below is only meaningful if this side of
// the contradiction is a fact about the file rather than a belief about it.
export function executesTests(workflow) {
  // `node --test` is in this list because it is now how the entire
  // repo-guards job runs: retiring the 46 per-guard `test:<name>` package.json
  // wrappers moved ~46 steps from `bun run test:<x>` to `node --test
  // scripts/test/<x>`, and a banner-honesty check blind to the spelling CI
  // actually uses would keep passing while seeing a fraction of the tests.
  const patterns = [
    /\bbun\s+run\s+test\b/,
    /\bbun\s+test\b/,
    /\bnode\s+--test\b/,
    /\bcargo\s+test\b/,
    /\bvitest\b/,
  ]
  return runStepLines(workflow).filter((line) => patterns.some((p) => p.test(line)))
}

// Claims a workflow may not make while it runs tests. Matched over the WHOLE
// file, comments included — a banner IS a comment, and the banner is the
// thing that lied.
const TESTS_ARE_OFF_CLAIMS = [
  /TESTS PARKED/i,
  /All CI test execution is deliberately disabled/i,
  /test execution is (?:deliberately )?disabled/i,
]

// A job NAME may not assert a count. `Repo guards (seventeen node --test
// invariants)` was accurate for exactly as long as there were seventeen; it
// then described forty-two of them for months. Counts belong in a derived
// instrument (`scripts/guard-count.mjs` prints DERIVED_GUARD_COUNT), never in
// a string a human has to remember to edit.
const COUNT_WORDS =
  /\b(one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty)\b/i

export function jobNameLines(workflow) {
  // `name:` at four-space indent is a job's display name (a step's `- name:`
  // sits deeper and starts with a dash).
  return workflow
    .split('\n')
    .map((line, index) => ({ line, number: index + 1 }))
    .filter(({ line }) => /^ {4}name:\s*\S/.test(line))
    .map(({ line, number }) => ({ number, value: line.replace(/^ {4}name:\s*/, '').trim() }))
}

// ---------------------------------------------------------------------------
// E9-S6's three asked-for properties.
// ---------------------------------------------------------------------------

test('the workflow does not claim tests are disabled while it runs them', () => {
  const workflow = readCi()
  const testSteps = executesTests(workflow)
  assert.ok(
    testSteps.length > 0,
    'ci.yml resolves ZERO test-executing run: steps — either the extractor broke or CI genuinely runs no ' +
      'tests; both are findings, neither is a pass',
  )
  const claims = TESTS_ARE_OFF_CLAIMS.flatMap((pattern) => {
    const match = pattern.exec(workflow)
    return match ? [match[0]] : []
  })
  assert.deepEqual(
    claims,
    [],
    `ci.yml runs ${testSteps.length} test step(s) (e.g. \`${testSteps[0]}\`) while still stating ` +
      `${claims.map((c) => JSON.stringify(c)).join(', ')}. The banner and the jobs disagree; the jobs are ` +
      'the truth. Rewrite the banner (#751/G10) rather than deleting this assertion.',
  )
})

test('the job set is exactly the recorded one, both directions', () => {
  // A MAINTAINED list, deliberately — the same posture #960's build-host
  // allowlist takes. Deriving "the jobs that exist" from the file it is
  // checking would make this assertion true by construction. A job silently
  // dropped (or added without anyone deciding) fails here by name.
  const EXPECTED = [
    'guard',
    'build-chiefd',
    'chiefd-clippy',
    'chiefd-macos-check',
    'chiefd-release-check',
    'chiefd-checks',
    'licence-scan',
    'secret-scan',
    'typecheck',
    'lint-eslint',
    'lint-knip',
    'lint',
    'test-unit-base',
    'test-unit-chiefd',
    'test-unit-piing',
    'test-unit-piing-contract',
    'test-unit',
    'repo-guards-shard',
    'repo-guards-serial',
    'repo-guards',
    'cargo-test-workspace-shard',
    'cargo-test-workspace',
  ]
  const actual = jobNames(readCi())
  assert.deepEqual(
    [...actual].sort(),
    [...EXPECTED].sort(),
    `ci.yml's job set drifted. Present: ${actual.join(', ')}. Recorded: ${EXPECTED.join(', ')}. ` +
      'Update this list in the same commit that changes the workflow, with the reason in the commit message.',
  )
})

test('the cargo test matrix covers every workspace member exactly once', () => {
  const workflow = readCi()
  const expectedMembers = deriveExpectedCounts(join(repoRoot, 'apps', 'chiefd')).members
  for (const job of ['cargo-test-workspace-shard']) {
    const blockStart = workflow.indexOf(`\n  ${job}:\n`)
    const blockEnd = workflow.slice(blockStart + 1).search(/\n {2}[A-Za-z0-9][\w-]*:\n/)
    const block = workflow.slice(blockStart, blockEnd < 0 ? undefined : blockStart + 1 + blockEnd)
    const matrixMembers = [...block.matchAll(/^\s+members:\s+(.+)$/gm)].flatMap((match) => match[1].trim().split(/\s+/))
    // The matrix is compared against the members the Cargo workspace really
    // has. This is not circular: `expectedMembers` comes from apps/chiefd's
    // `[workspace] members`, never from ci.yml.
    assert.deepEqual(
      [...matrixMembers].sort(),
      [...expectedMembers].sort(),
      `${job} must cover every workspace member exactly once, and no member the workspace does not have`,
    )
    assert.equal(new Set(matrixMembers).size, matrixMembers.length, `${job} must not run a workspace member in two shards`)
  }
})

test('no job or step carries continue-on-error — a gate that cannot fail is not a gate', () => {
  const workflow = readCi()
  const offenders = workflow
    .split('\n')
    .map((line, index) => ({ line: line.trim(), number: index + 1 }))
    .filter(({ line }) => /^continue-on-error:/.test(line))
  assert.deepEqual(
    offenders.map((o) => `ci.yml:${o.number} ${o.line}`),
    [],
    'continue-on-error turns a red step into a green run',
  )
})

// ---------------------------------------------------------------------------
// The count-in-a-name class, killed rather than fixed once.
// ---------------------------------------------------------------------------

test('no job name asserts a count of anything', () => {
  const offenders = jobNameLines(readCi()).filter(({ value }) => COUNT_WORDS.test(value))
  assert.deepEqual(
    offenders.map((o) => `ci.yml:${o.number} name: ${o.value}`),
    [],
    'a spelled-out count in a job name is a number a human must remember to edit; it was wrong by a factor ' +
      'of two and a half for months. Say what the job checks, not how many.',
  )
})

test('self-check: the count-word pattern really fires on the exact name it was written for', () => {
  assert.ok(COUNT_WORDS.test('Repo guards (seventeen node --test invariants)'))
  assert.ok(!COUNT_WORDS.test('Repo guards (node --test repo invariants)'))
})

// ---------------------------------------------------------------------------
// Every job is gated and bounded.
// ---------------------------------------------------------------------------

test('every job except the docs-only guard depends on it and skips a docs-only pull request', () => {
  const workflow = readCi()
  const missing = []
  for (const job of jobNames(workflow)) {
    if (job === 'guard') continue
    const start = workflow.indexOf(`\n  ${job}:\n`)
    const rest = workflow.slice(start + 1)
    const nextJob = rest.slice(1).search(/\n {2}[A-Za-z0-9][\w-]*:\n/)
    const block = nextJob < 0 ? rest : rest.slice(0, nextJob + 1)
    if (!/\n\s*needs:.*\bguard\b/.test(block)) missing.push(`${job}: no \`needs:\` on the guard job`)
    if (!block.includes("needs.guard.outputs.docs-only != 'true'"))
      missing.push(`${job}: does not skip a docs-only pull request`)
    if (!/\n\s*timeout-minutes:\s*\d+/.test(block)) missing.push(`${job}: no timeout-minutes`)
  }
  assert.deepEqual(missing, [], missing.join('\n'))
})

// ---------------------------------------------------------------------------
// Ported verbatim from tests/ci-workflow.test.ts (#768, #918), whose triage
// disposition was keep:active. Same assertions, now in a lane that runs.
// ---------------------------------------------------------------------------

test('#768: every CI binary build and artifact site includes chief, chiefd AND beacond', () => {
  // THREE binaries since P6, not two: `chief` (the operator client) and
  // `chiefd` (the backend) are separate programs, and an artifact
  // carrying only the client produces a CI job where every operator verb
  // works until something starts a company. That is the same defect #768 was
  // opened for — a CI site that built one of a pair — one binary later.
  const workflow = readCi()
  assert.equal(
    workflow.match(
      /name: Build optimized CI debug-path binaries[\s\S]*?run: cargo build --locked --manifest-path apps\/chiefd\/Cargo\.toml --bin chief --bin chiefd --bin beacond/g,
    )?.length,
    1,
  )
  assert.match(workflow, /RUSTFLAGS: "-C opt-level=2"/)
  assert.match(workflow, /CARGO_PROFILE_DEV_DEBUG: "0"/)
  assert.match(workflow, /CARGO_PROFILE_TEST_DEBUG: "0"/)
  assert.match(workflow, /CARGO_PROFILE_TEST_OPT_LEVEL: "0"/)
  assert.ok(
    workflow.includes(
      'apps/chiefd/target/debug/chief\n            apps/chiefd/target/debug/chiefd\n            apps/chiefd/target/debug/beacond',
    ),
  )
  assert.match(workflow, /name: Check release profile with warnings denied\n        env:\n          RUSTFLAGS: -D warnings\n        run: cargo check --release/)
  assert.doesNotMatch(workflow, /cargo build --release --locked --manifest-path apps\/chiefd\/Cargo\.toml --bin chief/)
})

test('#768: every artifact consumer restores all executable bits', () => {
  const workflow = readCi()
  const declarations = workflow.match(/name: chiefd-ci-binary/g) ?? []
  const dualChmods =
    workflow.match(
      /chmod \+x apps\/chiefd\/target\/debug\/chief apps\/chiefd\/target\/debug\/chiefd apps\/chiefd\/target\/debug\/beacond/g,
    ) ?? []
  assert.equal(declarations.length, 3, 'one upload declaration and two binary-dependent consumers')
  assert.equal(dualChmods.length, 3, 'the producer and two binary-dependent consumers restore executable bits')
})

test('#918: build-chiefd writes the content-hash manifest BEFORE the upload, and the upload carries it', () => {
  const workflow = readCi()
  const buildChiefdStart = workflow.indexOf('\n  build-chiefd:\n')
  const chiefdChecksStart = workflow.indexOf('\n  chiefd-checks:\n')
  assert.ok(buildChiefdStart > -1)
  assert.ok(chiefdChecksStart > buildChiefdStart)
  const block = workflow.slice(buildChiefdStart, chiefdChecksStart)

  assert.ok(
    block.includes(
      'node scripts/prebuilt-binary-manifest.mjs write --binary-dir apps/chiefd/target/debug --binaries chief,chiefd,beacond',
    ),
  )
  const writeIndex = block.indexOf('Write prebuilt-binary content-hash manifest')
  const uploadIndex = block.indexOf('Upload chiefd CI binary')
  assert.ok(writeIndex > -1)
  assert.ok(uploadIndex > writeIndex, 'an upload racing ahead of the write ships binaries with no manifest')
  assert.ok(block.includes('apps/chiefd/target/debug/prebuilt-binary-manifest.json'))
})

test('#918: repo-guards runs the prebuilt-binary-manifest unit suite', () => {
  // The guard is selected by the derived runner, not repeated as a workflow
  // line. The manifest is the file-level wiring record.
  assert.equal(GUARD_WIRING_MANIFEST['prebuilt-binary-manifest.test.mjs']?.status, 'wired')
  assert.match(readCi(), /node scripts\/ci-guard-shards\.mjs --shards \d+/)
})

// ---------------------------------------------------------------------------
// The concurrency clause ci.yml's own comment says this file protects. It
// said so while asserting nothing of the kind — the comment named a test that
// did not contain the check. Now it does.
// ---------------------------------------------------------------------------

test('the concurrency group keeps branch isolation and supersedes stale PR runs', () => {
  const workflow = readCi()
  const group = /\n\s*group:\s*(.+)/.exec(workflow)?.[1] ?? ''
  assert.ok(group.length > 0, 'no concurrency group found')
  assert.ok(group.includes("github.event.pull_request.number && 'shared' || github.sha"))
  assert.doesNotMatch(group, /github\.head_ref/, 'all pull requests must share one group and cancel their stale run')
  assert.ok(
    /cancel-in-progress:\s*true\s*$/m.test(workflow),
    'cancel-in-progress must be a literal true. Every pull request collapses to `shared`, where superseding a stale run is the point; different non-PR commits use different SHA groups.',
  )
})

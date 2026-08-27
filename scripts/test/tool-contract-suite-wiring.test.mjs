// #751/P4: the organization tool-contract suite is the ONLY thing in CI that
// calls a tool. Everything else drives a pure unit or an HTTP route, and on
// 2026-08-09 that gap shipped three broken packets in one day — each one
// proved `POST /v1/org/department/create` returned 200 and each one was wrong,
// because the tool calls the route and THEN reconciles and classifies. Both
// defects lived after the 200.
//
// A suite that valuable degrades in exactly three ways, and this guard exists
// to make each of them a build failure rather than a quiet loss of coverage:
//
//   1. IT LEARNS TO SKIP. The nine `packages/chiefing/test/contract` suites
//      gate on `chiefdBinaryTestGate()` and skip when the debug test binary is
//      absent. That convention is precisely why they sat silently unrun until
//      `ContractSuiteResidual` had to be invented to name them, and why a
//      707/1/38 run read as green. If this suite can skip, it will skip.
//   2. IT STOPS CALLING TOOLS. A suite that quietly becomes another route test
//      keeps its name and its green tick and covers nothing new.
//   3. ITS DAEMON LOSES THE TMUX HOST. `chiefd run --serve-only` answers
//      `503 this chiefd has no tmux host capability` on `/v1/org/runtime/launch`
//      — the route every org tool calls after its durable write commits. A
//      previous proof used `--serve-only` and could not reach the code under
//      test at all.
//
// Derived from the files themselves, never from a hand-kept list.
//
// Run with `node --test scripts/test/tool-contract-suite-wiring.test.mjs`.

import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

const SUITE = join('packages', 'piing', 'test', 'toolcontract', 'OrganizationToolContract.test.ts')
const REMINDER_SUITE = join(
  'packages',
  'piing',
  'test',
  'toolcontract',
  'ReminderDeliveryContract.test.ts'
)
const SURFACE = join('packages', 'piing', 'test', 'support', 'OrganizationToolSurface.ts')
const HARNESS = join('packages', 'testing', 'src', 'TmuxHostedCompanyDaemon.ts')
const ORDERED_LANE_PATTERN =
  'the organization tools|durable reminders|the last four families|caller authentication'
const INDEPENDENT_LANE_PATTERNS = [
  'post-commit convergence|refusal reaches|web-hosted person|two companies',
  'reminder may be armed|structural family',
  'operator escalation|baseline mailbox|department end-of-life|contract family|staffing and placement'
]

/**
 * Strip comments before scanning for banned CODE.
 *
 * The suite's own header explains at length WHY it does not use
 * `chiefdBinaryTestGate()` and what `describe.skip` cost the nine contract
 * suites. A guard that matched that prose would forbid the file from
 * documenting the very decision this guard enforces — so it reads code only.
 */
function withoutComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, ' ').replace(/(^|[^:])\/\/.*$/gm, '$1')
}

function read(relativePath) {
  const absolute = join(repoRoot, relativePath)
  assert.ok(
    existsSync(absolute),
    `${relativePath} is missing. It is the only tool-level coverage in CI; ` +
      `if it moved, move this guard's path with it rather than deleting either.`
  )
  return readFileSync(absolute, 'utf8')
}

test('the tool-contract suite exists and installs the real extension', () => {
  const suite = read(SUITE)
  const reminderSuite = read(REMINDER_SUITE)
  const surface = read(SURFACE)

  assert.match(
    surface,
    /installOrganizationIntercom\(/,
    `${SURFACE} must install the REAL organization-intercom extension. ` +
      `A hand-written stand-in would prove nothing about the tool surface.`
  )
  assert.match(
    surface,
    /tool\.execute\(/,
    `${SURFACE} must invoke each tool's REGISTERED execute. If this fixture ` +
      `ends in an HTTP POST it has not closed the gap it exists to close.`
  )
  assert.match(
    suite,
    /installOrganizationToolSurface\(/,
    `${SUITE} must drive the tool surface, not talk to routes directly.`
  )
  assert.match(
    reminderSuite,
    /installOrganizationToolSurface\(/,
    `${REMINDER_SUITE} must drive the real tool surface, not talk to routes directly.`
  )
})

test('the isolated reminder delivery proof remains real and non-skippable', () => {
  const suite = withoutComments(read(REMINDER_SUITE))
  assert.match(
    suite,
    /assertChiefdBinaryBuilt\(/,
    `${REMINDER_SUITE} must fail when the current debug binaries are absent.`
  )
  assert.match(
    suite,
    /'org_create_reminder'/,
    `${REMINDER_SUITE} must exercise the registered create-reminder tool.`
  )
  assert.match(
    suite,
    /'org_list_reminders'/,
    `${REMINDER_SUITE} must read delivery through the registered list-reminders tool.`
  )
  for (const pattern of [/describe\.skip/, /it\.skip/, /test\.skip/, /chiefdBinaryTestGate/]) {
    assert.ok(
      !pattern.test(suite),
      `${REMINDER_SUITE} matched ${pattern} in CODE. The isolated proof must fail, never skip.`
    )
  }
})

test('the suite drives the tool families both #751/P4 defects hid in', () => {
  const suite = read(SUITE)
  // The exact tools that failed AFTER their route returned 200: the reconcile
  // defect (d2b235c90) and the staffing `applied`-key defect (abfaf6d11).
  for (const tool of [
    'org_launch_department',
    'org_lifecycle_status',
    // `org_loan` and `org_return` stood here until 2026-08-13. The operator
    // ruled the loan concept out of existence, so the tools are deleted, not
    // merely unused — a guard demanding the suite still call them would demand
    // a call to something that cannot be called. The remaining three carry the
    // same protection for the defects that are still reachable.
    'org_offboard'
  ]) {
    assert.ok(
      suite.includes(`'${tool}'`),
      `${SUITE} no longer calls ${tool}. That tool failed in production after ` +
        `its route answered 200; dropping it re-opens the gap.`
    )
  }
})

test('the four CI lanes cover every stateful contract family exactly once', () => {
  const suite = withoutComments(read(SUITE))
  const families = [...suite.matchAll(/^describe\('([^']+)'/gm)].map((match) => match[1])
  assert.equal(families.length, 14, `${SUITE} must keep its 14 top-level contract families`)

  const ordered = new RegExp(ORDERED_LANE_PATTERN)
  const independent = INDEPENDENT_LANE_PATTERNS.map((pattern) => new RegExp(pattern))
  const orderedFamilies = families.filter((family) => ordered.test(family))
  const independentFamilies = independent.map((pattern) =>
    families.filter((family) => pattern.test(family))
  )
  const coverage = families.map((family) =>
    [ordered, ...independent].filter((pattern) => pattern.test(family))
  )
  const uncovered = families.filter((_, index) => coverage[index].length === 0)
  const overlap = families.filter((_, index) => coverage[index].length > 1)

  assert.equal(orderedFamilies.length, 4, `ordered lane covers: ${orderedFamilies.join('; ')}`)
  // 3 + 2 + 5, plus the ordered lane's 4, is exactly the 14 families above.
  // Was 3 + 5 + 7 over 20: the durable-goal family and the
  // delegation/blocked-switch family were deleted with the goals feature, so
  // lane 2's pattern lost those two alternatives here and in ci.yml in the
  // same commit. Then 3 + 3 + 7 over 18 -> 3 + 2 + 5 over 14 with
  // provider/model management: the Pi-session-lifecycle family (whose only
  // subject was the model/thinking session guard), the model switch, the
  // thinking-effort family and the model-changed family are all deleted, and
  // all four alternatives leave the patterns here and in ci.yml together.
  assert.deepEqual(independentFamilies.map((lane) => lane.length), [3, 2, 5])
  assert.deepEqual(uncovered, [], `contract families not covered by a CI lane: ${uncovered.join('; ')}`)
  assert.deepEqual(overlap, [], `contract families covered by both CI lanes: ${overlap.join('; ')}`)

  const workflow = read(join('.github', 'workflows', 'ci.yml'))
  assert.ok(
    workflow.includes(ORDERED_LANE_PATTERN),
    'ci.yml changed the ordered contract pattern without updating this guard'
  )
  assert.ok(
    INDEPENDENT_LANE_PATTERNS.every((pattern) => workflow.includes(pattern)),
    'ci.yml changed an independent contract pattern without updating this guard'
  )
  assert.match(
    suite,
    /contractLane\.startsWith\('independent-'\)/,
    `${SUITE} must create the real Research fixture for each independent lane`
  )
  assert.match(
    suite,
    /name: 'Research'/,
    `${SUITE} independent lane bootstrap must create Research with the normal tool`
  )
})

test('the suite has no skip branch — a missing binary is a red, never a pass', () => {
  const suite = read(SUITE)
  const code = withoutComments(suite)
  for (const pattern of [/describe\.skip/, /it\.skip/, /test\.skip/, /chiefdBinaryTestGate/]) {
    assert.ok(
      !pattern.test(code),
      `${SUITE} matched ${pattern} in CODE. This suite must never gate itself into a ` +
        `skip: that convention is why nine real-binary contract suites sat ` +
        `silently unrun. It calls assertChiefdBinaryBuilt and fails instead.`
    )
  }
  assert.match(
    code,
    /assertChiefdBinaryBuilt\(/,
    `${SUITE} must assert the debug test binary is built, so an unbuilt machine ` +
      `gets a named red with the build command rather than a silent skip.`
  )
})

test('the suite boots a fully-actuating chiefd run, never --serve-only', () => {
  const harness = withoutComments(read(HARNESS))
  assert.ok(
    !/'--serve-only'/.test(harness),
    `${HARNESS} passes --serve-only. That mode leaves host_executor unset, so ` +
      `/v1/org/runtime/launch answers 503 and the reconcile step every org ` +
      `tool runs is unreachable — the exact blind spot this suite removes.`
  )
  assert.match(
    harness,
    /'run',\s*\n?\s*'--dir'/,
    `${HARNESS} must spawn a full \`chiefd run --dir <dir>\`.`
  )
  assert.ok(
    !/'--company'/.test(harness) && !/'--data-root'/.test(harness),
    `${HARNESS} still names a company by slug and data root. A company IS the ` +
      `directory it occupies; \`--company\`/\`--data-root\` are deleted, and a ` +
      `harness that passed either would be proving a command line no daemon parses.`
  )
})

test('the suite is inside a vitest include that `bun run test` actually fans out to', () => {
  const packageJson = JSON.parse(read(join('packages', 'piing', 'package.json')))
  assert.equal(
    packageJson.scripts?.['test:unit'],
    'vitest run',
    `@chief/piing must declare a \`test:unit\` script — that is what \`bun run ` +
      `test\` (turbo) and CI's test-unit job fan out to. Without it the suite ` +
      `is a file nobody runs.`
  )
  const vitestConfig = read(join('packages', 'piing', 'vitest.config.ts'))
  assert.match(
    vitestConfig,
    /include:\s*\['test\/\*\*\/\*\.test\.ts'\]/,
    `@chief/piing's vitest include no longer covers test/**; the tool-contract ` +
      `suite would stop running without any file being deleted.`
  )
  assert.ok(
    !vitestConfig.includes('OrganizationToolContract'),
    `@chief/piing's vitest config names the tool-contract suite — the only ` +
      `reason to name a file there is to exclude it. It must not be excluded.`
  )
})

test("CI's test-unit job provisions everything the suite needs", () => {
  const workflow = read(join('.github', 'workflows', 'ci.yml'))
  // The debug test binaries — chief, chiefd AND beacond. A full run is
  // served by `chiefd` (P6 split it out of the front door) and takes beacond
  // admission before it opens any storage, so all three must be executable.
  assert.match(
    workflow,
    /chmod \+x apps\/chiefd\/target\/debug\/chief apps\/chiefd\/target\/debug\/chiefd apps\/chiefd\/target\/debug\/beacond/,
    `CI must make chief, chiefd and beacond executable: a full \`chiefd run\` ` +
      `is served by the daemon binary and refuses to start without beacond admission.`
  )
  assert.match(
    workflow,
    /run: tmux -V/,
    `CI's test-unit job must prove tmux is present before running the suite — ` +
      `a full \`chiefd run\` actuates a real tmux host.`
  )
})

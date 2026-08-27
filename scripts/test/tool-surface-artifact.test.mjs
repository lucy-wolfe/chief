// THE TOOL SURFACE, ASSERTED AT THE ARTIFACT.
//
// A verification engineer ran the product for real and found two
// product-breaking defects that every suite in this repo was green over. Both
// had one root cause, and it is worth quoting:
//
//     The suites test the LIST of tools chiefd publishes.
//     Nobody tested the ARTIFACT handed to a provider.
//
// That is how three facts coexisted with a green board for a whole workstream:
//
//   1. the launch profile names 60 tools for a converged CEO;
//   2. the web host builds SEVEN of them and reports 53 as unavailable, so a
//      hosted CEO can read a file and cannot hire, delegate, or set a goal;
//   3. three tools serialize their parameters as `{"anyOf":[...]}` with no
//      top-level `type`. A strict provider rejects that definition outright,
//      and rejecting one definition kills the WHOLE catalog — every other tool
//      goes with it.
//
// None of the three is visible from a list of names, which is all any existing
// suite reads. This guard measures the artifact instead:
//
//   * `scripts/tool-surface-artifact.ts` builds the real surface in one
//     process — the real extensions' real `pi.registerTool` definitions, and
//     `apps/web`'s own `selectTools`, whose `unavailable` IS the value the
//     roster publishes as `degraded[].missingTools`. Nothing here recomputes
//     that fact a second way.
//   * this file asserts two properties of what came back.
//
// WHY IT ENUMERATES AND NEVER NAMES. A test that named the three malformed
// tools would not stop the fourth, and would have to be edited — quietly
// widening — every time a tool was added. The schema check walks every tool
// object that exists in the probe's process and reports whichever ones fail.
//
// WHY THERE ARE FLOORS. This repo's recurring defect is an instrument that
// cannot see its subject: a vacuity floor that passed because a nested runner
// skipped every file, a guard that asserted against its own source. An install
// that registered nothing, or a grant that shrank to a handful of ids, would
// make both assertions below true and meaningless. The floors are what make
// "everything passed" impossible to reach by seeing nothing.
//
// Run with `node --test scripts/test/tool-surface-artifact.test.mjs`.

import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const PROBE = join('scripts', 'tool-surface-artifact.ts')

/**
 * The number of tools a converged CEO's launch profile declares.
 *
 * chiefd's own actuator builds it (`person_tool_names`,
 * `chiefd-host/src/converge_apply/resource_catalog.rs`) from the person
 * record's coding, intercom baseline, manager and root-executive tools. A
 * FLOOR rather than an equality, so adding a tool to the
 * product never fails this guard — only losing one does, which is the
 * direction that has actually gone wrong.
 */
// Lowered 59 -> 48: eleven tools were removed from the product, so the
// declared grant genuinely shrank by that much. A capability disappearing
// without this number moving in the same commit is still the failure this
// guard exists to catch.
// Lowered 48 -> 42: the goals feature took six more with it — org_set_goal,
// org_complete_goal, org_set_goal_priority, org_delegate, org_clear_goals and
// org_set_assignment_blocked — removed from all four grant lists in one
// commit. The host now builds 42, which is the measured floor, not a guess.
// Lowered 42 -> 40: the loan concept was ruled out of the product, taking
// org_loan and org_return with it. There is no replacement verb — a move is
// org_transfer, and somebody who wants a person back asks for them — so this
// is a capability the product no longer has rather than one that moved.
// Lowered 40 -> 38: the tavily-search capability was deleted on operator
// ruling, taking tavily_search and tavily_extract out of the baseline grant.
// The number moves WITH its subject — the product lost exactly two tools —
// and the guard is not relaxed: 38 is still the measured floor.
// Lowered 38 -> 35: provider/model management is deleted, taking
// org_change_model, org_change_thinking and org_set_thinking out of the
// active-runtime and subtree grants. Chief chooses no model, so there is no
// replacement verb — three capabilities the product no longer has.
const DECLARED_TOOL_FLOOR = 35

/**
 * How many tools the real extensions must register for that same person.
 *
 * All of them from `organization-intercom`, the one extension the probe
 * installs. This is the vacuity floor for the schema check: an install that
 * silently produced a WORKER — or that failed halfway and registered nothing —
 * would otherwise satisfy "every registered tool has a well-formed schema" by
 * registering almost none.
 *
 * Kept level with what the probe really installs. Leaving the floor a tool short of what the probe registers would
 * let one vanish again without a red.
 */
// Lowered 52 -> 41 alongside DECLARED_TOOL_FLOOR above: one whole extension
// and its three registered tools were removed, and the intercom shed eight,
// taking it from forty-seven to thirty-nine.
// Lowered 41 -> 35 with the goals deletion: the intercom stopped registering
// the same six tools the declared grant lost.
// Lowered 35 -> 33 with the loan deletion: the intercom stopped registering
// org_loan and org_return, the same two the declared grant lost.
// Lowered 33 -> 31 with the tavily-search deletion: the probe no longer
// installs that extension at all, so the two tools it registered are gone
// from this count as well as from the declared grant above.
// Lowered 31 -> 28 with provider/model management: the intercom stopped
// registering the same three tools the declared grant lost.
const REGISTERED_TOOL_FLOOR = 28

/** The probe's document, or a named refusal.
 *
 * Runs once and is shared: it starts a loopback server and installs the real
 * extension, so paying for it per test would triple the cost for nothing. */
function surface() {
  // Package `exports` resolve to `dist/`, so the extensions cannot be imported
  // at all until the workspace packages are built. An unbuilt tree must say so
  // in those words rather than surfacing as a module-resolution stack trace
  // that reads like a deleted file.
  for (const built of ['packages/piing/dist', 'packages/chiefing/dist']) {
    assert.ok(
      existsSync(join(repoRoot, built)),
      `${built} is missing, so the probe cannot import the real extensions. ` +
        `Build the workspace packages first: bun x turbo run build --filter='./packages/*'`
    )
  }

  const run = spawnSync('bun', [PROBE], {
    cwd: repoRoot,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    timeout: 180_000
  })
  // The exit code is read off the spawn itself. Piping the probe through
  // anything would report the PIPE's status, which is the mistake that has
  // already made a red cargo run look green in this repo.
  assert.equal(
    run.status,
    0,
    `${PROBE} did not complete (status ${run.status}, signal ${run.signal}).\n` +
      `stdout:\n${(run.stdout ?? '').slice(-4000)}\nstderr:\n${(run.stderr ?? '').slice(-4000)}`
  )
  let document
  try {
    document = JSON.parse(run.stdout)
  } catch (error) {
    assert.fail(
      `${PROBE} did not print a JSON document (${error.message}).\n` +
        `stdout:\n${(run.stdout ?? '').slice(-4000)}\nstderr:\n${(run.stderr ?? '').slice(-4000)}`
    )
  }
  return document
}

const artifact = surface()

test('both halves of this comparison come from ONE checkout', () => {
  // FIRST, because every assertion below is a DIFF between two halves that are
  // read through two different resolution paths — the catalog by relative path
  // (this tree) and the extension by package name (through `node_modules`).
  // Both paths are correct and neither may be unified away; see `resolvedFrom`
  // in the probe for why. What they cannot be allowed to do is land in two
  // different checkouts, because then every difference between those two
  // revisions of the repo is reported as a product fault.
  //
  // It happened, and it is the reason this test exists: a worktree with a
  // symlinked `node_modules` resolved `@chief/piing` into the shared tree —
  // another agent's working copy, at an older commit — and this guard reported
  // `missing: ['org_resume', 'org_stand_down']`. That reads as "a hosted CEO
  // was granted the power to stop its own company and cannot use it", which is
  // alarming, specific, and entirely false: the tools were shipped, declared
  // and buildable, and the same commit passed in CI. Three agents were sent
  // after it.
  //
  // A guard that can be handed two repos should say so rather than diff them.
  assert.equal(
    artifact.resolvedFrom.extension,
    artifact.resolvedFrom.catalog,
    `this guard reads the tool CATALOG from\n  ${artifact.resolvedFrom.catalog}\n` +
      `and the EXTENSION the host installs from\n  ${artifact.resolvedFrom.extension}\n` +
      `which are two different checkouts, so NOTHING below is about this commit — any tool ` +
      `reported as missing is only a difference between two revisions of the repo.\n` +
      `Fix the worktree, not the test: run scripts/link-worktree-node-modules.sh`
  )
})

test('the declared tool surface has not shrunk out from under this guard', () => {
  // Asserted BEFORE the two properties below, because both are measured
  // against this grant. A grant that quietly collapsed would make each of them
  // trivially true — the exact shape of "an instrument that cannot see its
  // subject" this repo keeps paying for.
  assert.ok(
    artifact.grant.length >= DECLARED_TOOL_FLOOR,
    `the launch profile now declares only ${artifact.grant.length} tools for a CEO, below the ` +
      `${DECLARED_TOOL_FLOOR} this guard measures against. If tools were deliberately removed from ` +
      `the product, lower DECLARED_TOOL_FLOOR in the same commit and say why. If they were not, a ` +
      `capability just disappeared from every converged manager.`
  )
  assert.ok(
    artifact.registered.length >= REGISTERED_TOOL_FLOOR,
    `the real extensions registered only ${artifact.registered.length} tools, below the ` +
      `${REGISTERED_TOOL_FLOOR} floor. Either the install stopped short — in which case the schema ` +
      `check below is inspecting a fraction of the surface and proves nothing — or tools were removed.`
  )
})

test('the host BUILDS every tool the launch profile declares', () => {
  // `artifact.missing` is `selectTools`'s own `unavailable`, which is exactly
  // what `convergeRoster` publishes as `degraded[].missingTools`. That
  // measurement has existed all along and nothing asserted on it, so a host
  // that built 7 of 60 shipped with a roster politely reporting the other 53.
  //
  // WHAT MAKES THIS GREEN: the `ExtensionAPI` -> `AgentTool[]` adapter. The
  // `org_*` family registers its tools into Pi's `ExtensionAPI` while the web
  // host builds a flat `AgentTool[]`, and nothing bridges the two. Until it
  // does, a hosted CEO is a chat box wearing a CEO's name.
  // The check above already fails when the two halves are different checkouts,
  // but node:test runs every subtest, so this one still speaks. It must not be
  // allowed to say "a granted capability is unbuildable" without saying that
  // its own inputs were two repos — that sentence is what sent three agents
  // after a phantom. The assertion itself is UNCHANGED; only the message
  // learns to name the condition that makes it meaningless.
  const oneCheckout = artifact.resolvedFrom.extension === artifact.resolvedFrom.catalog
  assert.deepEqual(
    artifact.missing,
    [],
    `the host builds ${artifact.built.length} of the ${artifact.grant.length} tools this person is ` +
      `granted and cannot supply ${artifact.missing.length}. Every id listed here is a capability the ` +
      `company granted and the agent does not have:\n  ${artifact.missing.join('\n  ')}` +
      (oneCheckout
        ? ''
        : `\n\nDISREGARD THE LIST ABOVE. The catalog was read from ${artifact.resolvedFrom.catalog} ` +
          `and the extension from ${artifact.resolvedFrom.extension}, so it is a diff between two ` +
          `revisions of the repo and not a product fault. Run scripts/link-worktree-node-modules.sh.`)
  )
  assert.ok(
    artifact.built.length >= DECLARED_TOOL_FLOOR,
    `the host built ${artifact.built.length} tools, below the ${DECLARED_TOOL_FLOOR} floor.`
  )
})

test('every tool a provider is shown has a top-level object schema', () => {
  // By ENUMERATION. A provider validates each tool definition's parameters as
  // a JSON Schema and rejects one without a top-level `type`; a rejected
  // definition takes the whole catalog with it, so one malformed tool disarms
  // every other tool the person has. `{"anyOf":[...]}` — a union at the top
  // level — is the shape that did it, and it is what a discriminated action
  // union serializes to unless it is wrapped.
  //
  // WHAT MAKES THIS GREEN: each offending tool giving its parameters a
  // top-level `type: "object"`.
  const offenders = artifact.schemas
    .filter((entry) => entry.schema === null || entry.schema.type !== 'object')
    .map((entry) => `${entry.name} (${entry.source}): ${JSON.stringify(entry.schema)?.slice(0, 120)}`)

  assert.deepEqual(
    offenders,
    [],
    `${offenders.length} of ${artifact.schemas.length} tool schemas have no top-level ` +
      `\`type: "object"\`. A strict provider rejects the whole catalog over any one of them, so ` +
      `these disarm every tool the person has, not just themselves:\n  ${offenders.join('\n  ')}`
  )
  assert.ok(
    artifact.schemas.length >= REGISTERED_TOOL_FLOOR,
    `only ${artifact.schemas.length} tool schemas were inspected, below the ${REGISTERED_TOOL_FLOOR} ` +
      `floor — this check saw almost nothing and would have passed over anything.`
  )
})

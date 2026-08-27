// #751/G3 — THE REVERSE DIRECTION NOBODY WAS CHECKING.
//
// `scripts/test/guard-wiring.test.mjs` checks manifest -> workflow: a guard
// marked `wired` must be found invoked in some `.github/workflows/*.yml`.
// Nothing checked the other way: a workflow step invoking `bun run <name>`
// for a script that package.json does not define. `bun run <unknown>` exits
// non-zero, so such a step does not silently no-op — it KILLS its job, and
// every step after it in that job never executes.
//
// That is not hypothetical. `.github/workflows/ci.yml` invoked
// `bun run test:ts-durable-store-route-registration` after commit 477061fa7
// deleted the script from package.json (correctly recording why in
// scripts/guard-wiring-manifest.mjs, but never touching the workflow). The
// `repo-guards` job — the one carrying the repo's entire invariant layer —
// therefore died at step 8 on EVERY run from that commit onward, and the
// ~34 guards after it never ran. Every one of them reported nothing, which
// looks exactly like every one of them passing. It took a human reading a
// red run to find it; no guard could see it, because no guard looked in this
// direction.
//
// WHAT THIS CHECKS: every command a `run:` step of any
// `.github/workflows/*.yml` NAMES must really resolve. Two spellings, one
// failure mode:
//   - `bun run <script>` must resolve to a real script in the root
//     package.json. `bun run <unknown>` exits non-zero.
//   - `node --test <path>` must resolve to a real file on disk. `node --test
//     <missing>` exits non-zero too — identically fatal to its job, and this
//     is now the ONLY spelling the repo-guards job uses. The 46 one-line
//     `"test:<guard>"` wrappers that used to sit between ci.yml and
//     `scripts/test/*.test.mjs` are gone (they existed for no reason but to
//     give this job and `guard-wiring-manifest.mjs` a name to say, while the
//     real corpus — `scripts/gate-matrix-legs.mjs` — derived the files and
//     ran `node --test` on them directly). Deleting the wrappers without
//     teaching this guard the new spelling would have moved ~46 CI steps out
//     of its sight in the same commit that made them the whole job.
// A violation names the workflow file, the line, the step name, and the
// offending target — enough to fix without opening anything.
//
// SCOPE, DELIBERATELY: only `run:` step bodies are scanned, never comments.
// ci.yml's prose legitimately cites script names in backticks (including
// deleted ones, in the very comment explaining why a step was removed), and a
// guard that cannot tell a citation from an invocation would make writing an
// honest comment a build failure. Shell comments INSIDE a `run:` block are
// stripped for the same reason.
//
// NOT IN SCOPE: `bun x <tool>`, `bash scripts/*.sh`, `node scripts/*.mjs`
// (no `--test`), `cargo ...`. Those resolve against a package registry, or
// are already derived as the shell-gate category by
// `scripts/guard-count.mjs`, and a missing shell script fails loudly on its
// own with a path in the message.
//
// Run with `node --test scripts/test/workflow-script-resolution.test.mjs`.

import assert from 'node:assert/strict'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { GUARD_WIRING_MANIFEST } from '../guard-wiring-manifest.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const workflowsDir = join(repoRoot, '.github', 'workflows')
const packageJsonPath = join(repoRoot, 'package.json')

// `bun run <name>` where <name> is a package.json script name. Requires the
// literal `bun` immediately before `run`, so `bun x turbo run build` (and any
// other tool with a `run` subcommand) is not mistaken for one.
const BUN_RUN_PATTERN = /\bbun\s+run\s+([A-Za-z0-9][\w:.-]*)/g

// `node --test <path>`. The path is repo-relative in every workflow step
// today (`node --test scripts/test/x.test.mjs`); an absolute path or a
// `${{ }}` expression is deliberately NOT matched, because this guard checks
// paths it can resolve, and one it cannot resolve must not be reported as
// missing.
const NODE_TEST_PATTERN = /\bnode\s+--test\s+([A-Za-z0-9][\w./-]*)/g
const DERIVED_GUARD_RUNNER_PATTERN = /\bnode\s+(scripts\/ci-guard-shards\.mjs)\s+--shards\s+(\d+)/g

// ---------------------------------------------------------------------------
// Extraction. A deliberately small YAML-shape reader rather than a YAML
// parser dependency: what is needed is "which lines are inside a `run:`
// value, and what step do they belong to", which the indentation already
// says. A real parser would also lose the LINE NUMBER, which is most of what
// makes the failure message actionable.
// ---------------------------------------------------------------------------
export function extractRunCommands(workflowText, workflowName) {
  const lines = workflowText.split('\n')
  const commands = []
  let stepName = '(unnamed step)'
  let block = undefined

  const indentOf = (line) => line.length - line.replace(/^\s*/, '').length

  for (const [index, rawLine] of lines.entries()) {
    const lineNumber = index + 1
    const trimmed = rawLine.trim()

    // A block scalar continues until a line that is not blank and not more
    // indented than the `run:` key that opened it.
    if (block !== undefined) {
      if (trimmed.length === 0 || indentOf(rawLine) > block.indent) {
        if (!trimmed.startsWith('#')) {
          commands.push({ workflow: workflowName, line: lineNumber, step: block.step, text: rawLine })
        }
        continue
      }
      block = undefined
    }

    const nameMatch = /^-?\s*name:\s*(.+?)\s*$/.exec(trimmed.replace(/^-\s*/, '- '))
    if (nameMatch) {
      stepName = nameMatch[1]
      continue
    }

    const runMatch = /^(?:-\s*)?run:\s*(.*)$/.exec(trimmed)
    if (!runMatch) continue
    const value = runMatch[1].trim()
    if (value === '|' || value === '>' || value === '|-' || value === '>-') {
      block = { indent: indentOf(rawLine), step: stepName }
      continue
    }
    commands.push({ workflow: workflowName, line: lineNumber, step: stepName, text: value })
  }

  return commands
}

export function findBunRunInvocations(workflows) {
  const invocations = []
  for (const { name, text } of workflows) {
    for (const command of extractRunCommands(text, name)) {
      BUN_RUN_PATTERN.lastIndex = 0
      for (const match of command.text.matchAll(BUN_RUN_PATTERN)) {
        invocations.push({ ...command, script: match[1] })
      }
    }
  }
  return invocations
}

export function findNodeTestInvocations(workflows) {
  const invocations = []
  for (const { name, text } of workflows) {
    for (const command of extractRunCommands(text, name)) {
      NODE_TEST_PATTERN.lastIndex = 0
      for (const match of command.text.matchAll(NODE_TEST_PATTERN)) {
        invocations.push({ ...command, path: match[1] })
      }
    }
  }
  return invocations
}

export function findDerivedGuardRunnerInvocations(workflows) {
  const invocations = []
  for (const { name, text } of workflows) {
    for (const command of extractRunCommands(text, name)) {
      DERIVED_GUARD_RUNNER_PATTERN.lastIndex = 0
      for (const match of command.text.matchAll(DERIVED_GUARD_RUNNER_PATTERN)) {
        invocations.push({ ...command, path: match[1], shards: Number(match[2]) })
      }
    }
  }
  return invocations
}

// Pure validator so the real tree and a doctored fixture exercise identical
// logic — the shape guard-wiring.test.mjs and knip-workspace-map.test.mjs
// already use. `fileExists` is injected for the same reason `scripts` is
// passed in rather than read: a fixture must be able to describe a tree that
// does not exist on this disk.
export function validateWorkflowScriptResolution(workflows, scripts, fileExists = realFileExists) {
  const errors = []
  for (const invocation of findBunRunInvocations(workflows)) {
    if (typeof scripts[invocation.script] === 'string') continue
    errors.push(
      `.github/workflows/${invocation.workflow}:${invocation.line} — step "${invocation.step}" runs ` +
        `\`bun run ${invocation.script}\`, but package.json defines no "${invocation.script}" script. ` +
        `\`bun run <unknown>\` exits non-zero, so this step FAILS ITS WHOLE JOB and every step after ` +
        `it never runs (#751/G3: this is exactly how repo-guards stayed red).`
    )
  }
  for (const invocation of findNodeTestInvocations(workflows)) {
    if (fileExists(invocation.path)) continue
    errors.push(
      `.github/workflows/${invocation.workflow}:${invocation.line} — step "${invocation.step}" runs ` +
        `\`node --test ${invocation.path}\`, but no such file exists in the repo. ` +
        `\`node --test <missing>\` exits non-zero, so this step FAILS ITS WHOLE JOB and every step ` +
        `after it never runs — the same #751/G3 failure the \`bun run\` half above exists for, in the ` +
        `spelling repo-guards actually uses now.`
    )
  }
  for (const invocation of findDerivedGuardRunnerInvocations(workflows)) {
    if (fileExists(invocation.path)) continue
    errors.push(
      `.github/workflows/${invocation.workflow}:${invocation.line} — step "${invocation.step}" runs ` +
        `the derived guard runner ${invocation.path}, but no such file exists in the repo.`
    )
  }
  return errors
}

function realFileExists(repoRelativePath) {
  return existsSync(join(repoRoot, repoRelativePath))
}

function readWorkflows() {
  return readdirSync(workflowsDir)
    .filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
    .sort()
    .map((name) => ({ name, text: readFileSync(join(workflowsDir, name), 'utf8') }))
}

function readScripts() {
  return JSON.parse(readFileSync(packageJsonPath, 'utf8')).scripts
}

// ---------------------------------------------------------------------------
// The real gate.
// ---------------------------------------------------------------------------

// #848's floor, re-expressed over the COMBINED population. A workflow set
// that resolves zero invocations would make the check below pass while
// answering nothing — the same vacuous shape this whole file exists to make
// impossible.
//
// The floor used to count `bun run` invocations alone and sit at 10, when
// ci.yml carried 40+. Retiring the 46 per-guard `test:<name>` wrappers drops
// the `bun run` population to a handful (typecheck, lint, knip, test) while
// moving ~46 steps to `node --test` — the same steps, spelled directly. The
// floor is therefore raised, not lowered: it now counts every resolvable
// invocation this file checks, which is MORE than it saw before, and a
// collapse in EITHER extractor still trips it. Lowering the old number to
// match a shrunken half would have been the weakening this file's own
// #751/G3 header argues against.
const MINIMUM_RESOLVABLE_INVOCATIONS = 20

test('every `bun run <script>` and `node --test <file>` in a workflow step really resolves', () => {
  const workflows = readWorkflows()
  assert.ok(workflows.length > 0, '.github/workflows resolved zero workflow files — refusing to trust this scan')

  const bunRun = findBunRunInvocations(workflows)
  const nodeTest = findNodeTestInvocations(workflows)
  const derivedRunner = findDerivedGuardRunnerInvocations(workflows)
  const wiredGuardCount = Object.values(GUARD_WIRING_MANIFEST).filter((entry) => entry.status === 'wired').length
  const total = bunRun.length + nodeTest.length + (derivedRunner.length > 0 ? wiredGuardCount : 0)
  console.log(
    `[workflow-script-resolution] scanned ${workflows.length} workflow file(s), ` +
      `${bunRun.length} \`bun run\` + ${nodeTest.length} \`node --test\` invocation(s) in run: steps`
      + ` + ${derivedRunner.length} derived guard runner(s)`
  )
  assert.ok(
    total >= MINIMUM_RESOLVABLE_INVOCATIONS,
    `only ${total} resolvable invocation(s) found across ${workflows.length} workflow file(s) ` +
      `(floor ${MINIMUM_RESOLVABLE_INVOCATIONS}) — the extractor is probably broken, not the workflows. ` +
      'REFUSING TO TRUST THIS RESULT.'
  )
  assert.ok(
    bunRun.length > 0 && (nodeTest.length > 0 || derivedRunner.length > 0),
    `one whole spelling resolved to zero (bun run: ${bunRun.length}, node --test: ${nodeTest.length}, ` +
      `derived runner: ${derivedRunner.length}) — ` +
      'a half-blind extractor reports clean over the half it can still see, which is the exact shape ' +
      'this floor exists to refuse'
  )

  const errors = validateWorkflowScriptResolution(workflows, readScripts())
  assert.deepEqual(errors, [], errors.join('\n'))
})

// ---------------------------------------------------------------------------
// Self-tests. A guard that has never been seen to fail is indistinguishable
// from one that cannot.
// ---------------------------------------------------------------------------

test('RED: the exact #751/G3 defect — a step invoking a deleted script — fails, naming the step and the script', () => {
  const workflow = [
    'jobs:',
    '  repo-guards:',
    '    steps:',
    '      - name: SQL-only state',
    '        run: bun run test:sql-only-state',
    '      - name: ts durable store route registration',
    '        run: bun run test:ts-durable-store-route-registration',
    '',
  ].join('\n')
  const errors = validateWorkflowScriptResolution(
    [{ name: 'ci.yml', text: workflow }],
    { 'test:sql-only-state': 'node --test scripts/test/sql-only-state.test.mjs' },
    () => true
  )
  assert.equal(errors.length, 1, `expected exactly one violation, got: ${JSON.stringify(errors)}`)
  assert.match(errors[0], /ci\.yml:7/)
  assert.match(errors[0], /ts durable store route registration/)
  assert.match(errors[0], /test:ts-durable-store-route-registration/)
})

test('GREEN: adding the script back to package.json clears that same violation', () => {
  const workflow = 'jobs:\n  j:\n    steps:\n      - name: s\n        run: bun run test:brand-new\n'
  const withoutScript = validateWorkflowScriptResolution([{ name: 'ci.yml', text: workflow }], {}, () => true)
  assert.equal(withoutScript.length, 1)
  const withScript = validateWorkflowScriptResolution(
    [{ name: 'ci.yml', text: workflow }],
    { 'test:brand-new': 'node --test scripts/test/brand-new.test.mjs' },
    () => true
  )
  assert.deepEqual(withScript, [])
})

test('a `bun run` inside a multi-line `run: |` block is found, with its own line number', () => {
  const workflow = [
    'jobs:',
    '  j:',
    '    steps:',
    '      - name: multi',
    '        run: |',
    '          set -euo pipefail',
    '          bun run test:does-not-exist',
    '',
  ].join('\n')
  const invocations = findBunRunInvocations([{ name: 'ci.yml', text: workflow }])
  assert.equal(invocations.length, 1)
  assert.equal(invocations[0].script, 'test:does-not-exist')
  assert.equal(invocations[0].line, 7)
  assert.equal(invocations[0].step, 'multi')
})

test('a script name cited in a YAML comment is NOT an invocation — an honest comment must not fail a build', () => {
  const workflow = [
    'jobs:',
    '  j:',
    '    steps:',
    '      # REMOVED: `bun run test:ts-durable-store-route-registration` (#950).',
    '      - name: still here',
    '        run: bun run test:sql-only-state',
    '',
  ].join('\n')
  const invocations = findBunRunInvocations([{ name: 'ci.yml', text: workflow }])
  assert.deepEqual(invocations.map((i) => i.script), ['test:sql-only-state'])
})

test('a shell comment INSIDE a run: block is stripped too', () => {
  const workflow = [
    'jobs:',
    '  j:',
    '    steps:',
    '      - name: multi',
    '        run: |',
    '          # bun run test:a-name-that-is-only-prose',
    '          bun run typecheck',
    '',
  ].join('\n')
  const invocations = findBunRunInvocations([{ name: 'ci.yml', text: workflow }])
  assert.deepEqual(invocations.map((i) => i.script), ['typecheck'])
})

test('`bun x turbo run build` is not mistaken for a `bun run build` script invocation', () => {
  const workflow =
    "jobs:\n  j:\n    steps:\n      - name: s\n        run: bun x turbo run build --filter='./packages/*'\n"
  assert.deepEqual(findBunRunInvocations([{ name: 'ci.yml', text: workflow }]), [])
})

test('the extractor finds the real ci.yml step names, not just line text', () => {
  const workflows = readWorkflows()
  const invocations = [...findBunRunInvocations(workflows), ...findNodeTestInvocations(workflows)]
  assert.ok(
    workflows.some(({ text }) => /node scripts\/ci-guard-shards\.mjs --shards \d+/.test(text)),
    'expected the real repo-guards job to invoke the derived parallel guard runner'
  )
  assert.ok(
    findBunRunInvocations(workflows).some((i) => i.script === 'typecheck'),
    'expected the real typecheck job to invoke `bun run typecheck`'
  )
  assert.ok(
    invocations.every((i) => i.step.length > 0 && i.line > 0),
    'every invocation must carry a non-empty step name and a real line number'
  )
})

// ---------------------------------------------------------------------------
// The `node --test` half's own self-tests. This is the spelling repo-guards
// uses for every one of its ~46 steps, so a blind spot here is a blind spot
// over the entire repo-invariant layer.
// ---------------------------------------------------------------------------

test('RED: a step running `node --test` on a file that does not exist fails, naming the step and the path', () => {
  const workflow = [
    'jobs:',
    '  repo-guards:',
    '    steps:',
    '      - name: SQL-only state',
    '        run: node --test scripts/test/sql-only-state.test.mjs',
    '      - name: ts durable store route registration',
    '        run: node --test scripts/test/ts-durable-store-route-registration.test.mjs',
    '',
  ].join('\n')
  const errors = validateWorkflowScriptResolution(
    [{ name: 'ci.yml', text: workflow }],
    {},
    (path) => path === 'scripts/test/sql-only-state.test.mjs'
  )
  assert.equal(errors.length, 1, `expected exactly one violation, got: ${JSON.stringify(errors)}`)
  assert.match(errors[0], /ci\.yml:7/)
  assert.match(errors[0], /ts durable store route registration/)
  assert.match(errors[0], /ts-durable-store-route-registration\.test\.mjs/)
})

test('GREEN: the same step resolves once the file exists', () => {
  const workflow =
    'jobs:\n  j:\n    steps:\n      - name: s\n        run: node --test scripts/test/brand-new.test.mjs\n'
  assert.equal(validateWorkflowScriptResolution([{ name: 'ci.yml', text: workflow }], {}, () => false).length, 1)
  assert.deepEqual(validateWorkflowScriptResolution([{ name: 'ci.yml', text: workflow }], {}, () => true), [])
})

test('a `node --test` path cited in a comment is not an invocation, inside a run: block or outside one', () => {
  const workflow = [
    'jobs:',
    '  j:',
    '    steps:',
    '      # REMOVED: `node --test scripts/test/gone.test.mjs` (#950).',
    '      - name: multi',
    '        run: |',
    '          # node --test scripts/test/also-gone.test.mjs',
    '          node --test scripts/test/still-here.test.mjs',
    '',
  ].join('\n')
  assert.deepEqual(
    findNodeTestInvocations([{ name: 'ci.yml', text: workflow }]).map((i) => i.path),
    ['scripts/test/still-here.test.mjs']
  )
})

test('`node scripts/guard-count.mjs` (no --test) is not mistaken for a node --test invocation', () => {
  const workflow = "jobs:\n  j:\n    steps:\n      - name: s\n        run: node scripts/guard-count.mjs\n"
  assert.deepEqual(findNodeTestInvocations([{ name: 'ci.yml', text: workflow }]), [])
})

test('every real ci.yml `node --test` path resolves on this disk', () => {
  const invocations = findNodeTestInvocations(readWorkflows())
  const derived = findDerivedGuardRunnerInvocations(readWorkflows())
  assert.ok(
    invocations.length > 0 || derived.length > 0,
    'no node --test steps or derived guard runner found — the extractor, not the workflow, is the finding'
  )
  const missing = invocations.filter((i) => !existsSync(join(repoRoot, i.path)))
  assert.deepEqual(missing.map((i) => `${i.workflow}:${i.line} ${i.path}`), [])
  assert.deepEqual(derived.filter((i) => !existsSync(join(repoRoot, i.path))).map((i) => i.path), [])
})

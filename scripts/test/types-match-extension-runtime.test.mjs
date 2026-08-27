import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

// **`@types/node` DESCRIBES THE RUNTIME THE EXTENSIONS RUN ON, NOT CI's.**
//
// There are two Node runtimes in this product and one `@types/node` types both:
//
//  1. TOOLING — vitest, tsc, eslint, knip, the guards. That is CI's Node, and
//     it is deliberately ahead (pinned in `ci.yml`).
//  2. THE PI EXTENSIONS — `packages/piing/extensions/*.ts`, which ship as
//     SOURCE and are `import()`ed by Pi inside Pi's own process. Pi's bin is
//     `#!/usr/bin/env node` and its `engines.node` states the floor.
//
// Types must not lie about (2). At a major ahead of it, an extension author
// gets completion and type-checking for APIs the operator's Node does not have,
// and nothing stops them shipping one — the compiler would be describing a
// different program from the one that runs on the box.
//
// THE PAIRING THIS GUARD REFUSES TO ENCODE: types-major == CI's node-version.
// That was the first shape proposed, and it is backwards — under it, moving CI
// forward FORCES the types forward, which is exactly the divergence from
// production the guard exists to prevent. CI's runner image must not get to
// decide what the extensions' types claim.
//
// The floor is READ from Pi's own manifest rather than restated here, so the
// next `@types/node` bump asks "has the runtime our extensions execute on
// moved?" instead of being checked against a number somebody remembered.

const ROOT = fileURLToPath(new URL('../..', import.meta.url))
const PI_MANIFEST = join(ROOT, 'node_modules/@earendil-works/pi-coding-agent/package.json')

/** The major version of a semver-ish string. */
function major(version) {
  const found = /(\d+)\./.exec(String(version).replace(/^[^\d]*/, ''))
  return found ? Number(found[1]) : undefined
}

test('@types/node matches the Node major the Pi extensions run on', () => {
  // REFUSE IN WORDS rather than pass, when the subject cannot be read. An
  // install that has not run is a fact about this checkout, not evidence that
  // the versions agree.
  if (!existsSync(PI_MANIFEST)) {
    assert.fail(
      `CANNOT CHECK: ${PI_MANIFEST} is missing, so Pi's engines.node floor cannot be read. ` +
        'Run `bun install` (or `scripts/link-worktree-node-modules.sh` in a worktree) first.',
    )
  }
  const engines = JSON.parse(readFileSync(PI_MANIFEST, 'utf8')).engines?.node
  assert.ok(engines, "Pi's manifest must state engines.node; that floor is this guard's subject")
  const runtimeMajor = major(engines)
  assert.ok(runtimeMajor, `could not read a major out of Pi's engines.node ${JSON.stringify(engines)}`)

  for (const manifest of ['package.json', 'apps/web/package.json']) {
    const pkg = JSON.parse(readFileSync(join(ROOT, manifest), 'utf8'))
    const declared = pkg.devDependencies?.['@types/node'] ?? pkg.dependencies?.['@types/node']
    assert.ok(declared, `${manifest} must declare @types/node`)
    assert.equal(
      major(declared),
      runtimeMajor,
      `${manifest} declares @types/node ${declared}, but the Pi extensions run on Node ` +
        `${engines}. Types a major ahead describe APIs the operator's Node does not have, and ` +
        'nothing stops an extension author using one. Types a major behind describe a smaller ' +
        'program than the one that runs.',
    )
  }
})

test("CI's own Node is pinned, and is NOT what this guard compares against", () => {
  const workflow = readFileSync(join(ROOT, '.github/workflows/ci.yml'), 'utf8')
  const versions = [...workflow.matchAll(/^\s+node-version: '(\d+)'$/gm)].map((m) => m[1])
  // Pinned at all: the whole point is that CI's Node stops being whatever the
  // runner image happens to ship.
  assert.ok(versions.length > 0, "ci.yml must pin a node-version; an inherited Node is not a choice")
  // ONE version across the workflow. Two would be two answers to "what does CI
  // run", and the second is the one nobody checks.
  assert.deepEqual(
    [...new Set(versions)],
    [versions[0]],
    `ci.yml pins more than one node-version: ${JSON.stringify([...new Set(versions)])}`,
  )
  // AND IT IS ALLOWED TO DIFFER from the extension runtime. This assertion is
  // the guard stating its own scope: if a later change makes CI's version the
  // thing types are compared against, the arm above becomes a different guard
  // wearing this one's name.
  const declared = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).devDependencies[
    '@types/node'
  ]
  assert.ok(
    declared,
    'the types declaration is the subject of the first arm; this arm only proves it is not ' +
      "compared against CI's Node",
  )
})

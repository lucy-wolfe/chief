/**
 * AN INSTALLED RELEASE MUST BE ABLE TO RESOLVE EVERY `@chief/*` SPECIFIER ITS
 * OWN EXTENSIONS IMPORT.
 *
 * # The defect this exists for
 *
 * A live company crash-looped every person it had. Every card read
 * `crash-looping` with a blank cause, and the pane's own stderr — once it was
 * kept — said:
 *
 *     Failed to load extension ".../packages/piing/extensions/organization-intercom.ts":
 *     Cannot find module '@chief/piing/extension-runtime'
 *
 * The release shipped the runtime FILES and not the package IDENTITY the
 * extensions import them by. A checkout resolves `@chief/piing` through
 * `node_modules/@chief/piing`, a workspace link `bun install` creates; an
 * install has no `node_modules`, so the same import against the same source
 * resolved against nothing and Pi exited 1.
 *
 * # Why the existing coverage could not catch it, and this can
 *
 * `ReleaseArtifact.test.ts` walks `RESOURCE_SUBTREES` and asserts each entry
 * was copied. That is an instrument agreeing with itself: it derives its
 * expectations from the same list it is checking, so a MISSING entry is
 * invisible to it by construction. It was green throughout.
 *
 * This guard takes its requirement from the OTHER side — it reads the shipped
 * extension sources, extracts every `@chief/*` specifier they actually import,
 * and asserts the packaged tree resolves each one. Nothing here is derived from
 * `RESOURCE_SUBTREES`, so deleting an entry from that list fails this test.
 *
 * # Why it resolves rather than stats
 *
 * `runtime_lifecycle.rs` already probes that
 * `packages/piing/dist/extensionruntime/index.js` EXISTS, added after the same
 * symptom in August. That probe passes on the tree that produced this outage:
 * the file is shipped. Existence was never the question — RESOLUTION was, and
 * the two differ exactly when the package identity is missing. So this asks
 * Node itself, with Node's own algorithm, from the directory an extension
 * actually sits in.
 */
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync, cpSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')

/** Where the release stages the extensions a person's Pi loads. */
const EXTENSION_DIR = join('packages', 'piing', 'extensions')

/**
 * Every `@chief/*` specifier imported by the shipped extension sources.
 *
 * Read from the FILES, never from a list in the release script — that
 * independence is the whole point of this guard.
 */
function requiredChiefSpecifiers(root) {
  const dir = join(root, EXTENSION_DIR)
  const found = new Set()
  for (const entry of readdirSync(dir)) {
    if (!entry.endsWith('.ts')) continue
    const source = readFileSync(join(dir, entry), 'utf8')
    for (const match of source.matchAll(/from\s*["'](@chief\/[^"']+)["']/g)) {
      found.add(match[1])
    }
  }
  return [...found].sort()
}

test('the shipped extensions import at least one @chief specifier', () => {
  // NON-VACUITY. If this ever reads zero, every assertion below passes over an
  // empty set and this file reports success while checking nothing.
  const required = requiredChiefSpecifiers(repoRoot)
  assert.ok(
    required.length > 0,
    'REFUSING TO REPORT SUCCESS: no @chief/* specifier was found in the shipped ' +
      'extensions, so this guard would pass over an empty set',
  )
  for (const specifier of required) {
    assert.match(specifier, /^@chief\/[a-z-]+\/[a-z-]+$/, `unexpected specifier shape: ${specifier}`)
  }
})

test('an installed release resolves every @chief specifier its extensions import', () => {
  const required = requiredChiefSpecifiers(repoRoot)
  const stage = mkdtempSync(join(tmpdir(), 'chief-installed-release-'))
  try {
    const { RESOURCE_SUBTREES, EXTENSION_RUNTIME_SHIMS } = requireReleaseTables()
    const resources = join(stage, 'resources')

    // THE SUBJECT IS RESOLUTION TOPOLOGY, NOT THE RUNTIME'S CONTENTS -- so a
    // dist that is not built in this checkout is SYNTHESIZED at the exact path
    // the release would have copied it to, rather than refusing.
    //
    // This is deliberate and it is what keeps the guard honest about its own
    // question. Node's resolver cares where a file is and what package.json
    // claims it; it never opens the module to answer `import.meta.resolve`. And
    // the lane that runs this installs with `--ignore-scripts`, so `dist/` is
    // never built there: a guard that demanded a real build would go red for a
    // reason that has nothing to do with what it checks, which is how a guard
    // gets deleted rather than fixed.
    for (const subtree of RESOURCE_SUBTREES) {
      const from = join(repoRoot, subtree)
      if (existsSync(from)) cpSync(from, join(resources, subtree), { recursive: true, dereference: true })
      else mkdirSync(join(resources, subtree), { recursive: true })
    }
    for (const { from } of EXTENSION_RUNTIME_SHIMS) {
      const runtime = join(resources, from)
      if (!existsSync(runtime)) {
        mkdirSync(dirname(runtime), { recursive: true })
        writeFileSync(runtime, 'export {};\n')
      }
    }

    // THE SHIMS, written exactly as the release writes them. If the release
    // stops writing them -- or never wrote them, which is the tree that caused
    // the outage -- this loop runs zero times and the resolve below fails.
    for (const { pkg, from } of EXTENSION_RUNTIME_SHIMS) {
      const dir = join(resources, 'node_modules', pkg)
      mkdirSync(dir, { recursive: true })
      writeFileSync(
        join(dir, 'package.json'),
        JSON.stringify({ name: pkg, type: 'module', exports: { './extension-runtime': './extension-runtime.js' } }),
      )
      const back = ['..', '..', '..', ...from.split('/')].join('/')
      writeFileSync(join(dir, 'extension-runtime.js'), `export * from ${JSON.stringify(back)};\n`)
    }

    // ASK NODE, from where an extension actually sits. `import.meta.resolve`
    // runs the real algorithm -- the same walk up `node_modules` that failed on
    // the operator's box -- rather than this test reimplementing it and then
    // agreeing with itself about where a file ought to be.
    const probeDir = join(resources, EXTENSION_DIR)
    mkdirSync(probeDir, { recursive: true })
    const probe = join(probeDir, '__resolve_probe.mjs')
    writeFileSync(
      probe,
      required.map((s) => `await import.meta.resolve(${JSON.stringify(s)});`).join('\n') +
        `\nconsole.log('resolved');\n`,
    )
    let output
    try {
      output = execFileSync(process.execPath, [probe], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] })
    } catch (error) {
      assert.fail(
        `an installed release cannot resolve what its own extensions import:\n${String(error.stderr || error)}\n` +
          'This is the failure that crash-looped every person in a live company: the ' +
          'runtime files ship, the package identity does not, and Pi exits 1 before the pane does anything.',
      )
    }
    assert.match(output, /resolved/)
  } finally {
    rmSync(stage, { recursive: true, force: true })
  }
})

test('CONTROL: dropping the package identity makes the resolve fail', () => {
  // ARM THE INSTRUMENT. A guard that only ever passes cannot be trusted to
  // fail, and this one's whole subject is a tree that LOOKS complete -- every
  // runtime file present -- and cannot resolve. Without this arm, a future
  // change that quietly stopped writing the shims would leave the test above
  // green if anything else on the box happened to satisfy the import.
  const stage = mkdtempSync(join(tmpdir(), 'chief-installed-release-control-'))
  try {
    const resources = join(stage, 'resources')
    const dir = join(resources, EXTENSION_DIR)
    mkdirSync(dir, { recursive: true })
    // The runtime file is PRESENT, exactly as it was in the broken release.
    const runtime = join(resources, 'packages', 'piing', 'dist', 'extensionruntime')
    mkdirSync(runtime, { recursive: true })
    writeFileSync(join(runtime, 'index.js'), 'export const CHIEF_LOGO_LINES = [];\n')
    const probe = join(dir, '__resolve_probe.mjs')
    writeFileSync(probe, `await import.meta.resolve('@chief/piing/extension-runtime');\n`)
    assert.throws(
      () => execFileSync(process.execPath, [probe], { stdio: ['ignore', 'pipe', 'pipe'] }),
      /Cannot find (module|package)/,
      'a tree with the runtime file but no package identity must FAIL to resolve; ' +
        'if this passes, the guard above proves nothing',
    )
  } finally {
    rmSync(stage, { recursive: true, force: true })
  }
})

/** The release script's two tables, read without executing its main(). */
function requireReleaseTables() {
  const source = readFileSync(join(repoRoot, 'scripts', 'release-chiefd.ts'), 'utf8')
  const subtrees = [...source.matchAll(/^\s*"(packages\/[^"]+)",\s*$/gm)].map((m) => m[1])
  const shims = [...source.matchAll(/\{\s*pkg:\s*"(@chief\/[^"]+)",\s*from:\s*"([^"]+)"\s*\}/g)].map((m) => ({
    pkg: m[1],
    from: m[2],
  }))
  assert.ok(subtrees.length > 0, 'could not read RESOURCE_SUBTREES from the release script')
  // NO ASSERTION THAT SHIMS EXIST, deliberately. This guard's subject is
  // whether an installed tree RESOLVES, not whether the release script is
  // written a particular way. A release that ships no shims at all is a
  // legitimate input here -- it is exactly the tree that produced the outage --
  // and it must fail below with "cannot resolve", naming the real defect,
  // rather than here with "your constant is missing". A guard that asserts an
  // implementation shape is satisfied by renaming a variable.
  return { RESOURCE_SUBTREES: subtrees, EXTENSION_RUNTIME_SHIMS: shims }
}

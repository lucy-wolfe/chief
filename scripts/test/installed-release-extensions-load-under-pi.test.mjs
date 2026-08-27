/**
 * AN INSTALLED RELEASE'S EXTENSIONS MUST ACTUALLY LOAD, IN REAL PI.
 *
 * # The outage this exists for
 *
 * Three of a live company's people were dead panes, `status=1`, and Pi's own
 * words were:
 *
 *     Error: Failed to load extension ".../organization-intercom.ts":
 *       Cannot find module '@chief/piing/extension-runtime'
 *     Error: Failed to load extension ".../team-ui.ts":
 *       Cannot find module '@chief/chiefing/extension-runtime'
 *
 * The installed tree could not resolve the package identities its own shipped
 * extensions import. Pi exited 1, the pane died, and the converge layout step
 * then failed on the corpses.
 *
 * # WHY THIS GUARD USES THE REAL PACKAGER AND THE REAL PI, AND NOTHING ELSE
 *
 * The transferable lesson, and the reason the cheap version of this guard is
 * disqualified: **the defect lived in the GAP BETWEEN TWO RESOLVERS.**
 * `@chief/*` resolves under a checkout's `node_modules` and does not resolve
 * under the shipped tree. A guard that introduces a THIRD resolver — a
 * hand-rolled specifier walk, or `import.meta.resolve` under the test's own
 * runtime — is one more resolver that can disagree with Pi's, and
 * *"it resolves for my resolver"* is exactly the look-implemented shape that
 * shipped this bug in the first place. Every instrument here is therefore one
 * of the two that ship: the packager that builds the tree
 * (`assembleVersionTree`, called directly, never mirrored) and the loader that
 * loads it (`pi`, the real binary, the same one a person's pane runs).
 *
 * This is deliberately NOT a duplicate of
 * `installed-release-loads-its-extensions.test.mjs`. That guard asks Node
 * whether the specifier RESOLVES, cheaply, in the ordinary guard shard on
 * every pull request, and it stages synthesized `dist` files because its lane
 * installs `--ignore-scripts`. It proves the resolution topology. It cannot
 * prove the module LOADS, that its exports are what the extensions
 * destructure, or that Pi accepts it. This one proves that, with a real build
 * and a real Pi, and costs a lane of its own. Both are worth keeping and each
 * says so.
 *
 * # WHEN THIS GUARD SHOULD BE RETIRED, AND WHAT WOULD NOT RETIRE IT
 *
 * This guard is slow — it starts a real Pi several times — and it earns that
 * cost only for as long as Pi's loader can differ from Node's resolver. So the
 * retirement condition is written down rather than left to a future reader's
 * judgement:
 *
 *   **If somebody VERIFIES that Pi's loader delegates bare-specifier
 *   resolution to Node byte-for-byte, that verification retires this guard**
 *   and the cheap resolver-proxy next door becomes sufficient.
 *
 * **An ASSUMPTION that it delegates does not retire it, and neither does a
 * reading of Pi's source that was not run.** The distinction is the entire
 * subject of this file: the defect it exists for was two resolvers that were
 * believed to agree and did not. Delete this guard on evidence, never on
 * confidence.
 *
 * # `pi -ne` IS NOT A REMEDY AND MUST NEVER BE OFFERED AS ONE
 *
 * `--no-extensions` appears in the invocation below purely to keep the run
 * ISOLATED — only the one extension under test is loaded, so a failure names
 * that file. It is never the fix for a load failure in production: an
 * extension-less person is a person with no intercom, which is a person who
 * cannot be managed at all. Pi refusing to start on an extension it cannot
 * load is correct behaviour being correct.
 */
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'
import test from 'node:test'

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const piBinary = join(repoRoot, 'node_modules', '.bin', 'pi')
const releaseScript = join(repoRoot, 'scripts', 'release-chiefd.ts')

/** Every input the real packager needs before it can stage a tree at all. */
const PACKAGER_INPUTS = [
  'packages/piing/extensions',
  'packages/piing/skills',
  'packages/piing/dist/extensionruntime',
  'packages/chiefing/dist',
]

/**
 * Why this run cannot judge anything, in words — or `undefined` when it can.
 *
 * A guard that cannot run must SAY SO. Silently passing is how a dead check
 * and a live one become indistinguishable, which is the whole failure class
 * this file belongs to.
 */
function refusal() {
  if (!existsSync(piBinary)) {
    return `CANNOT CHECK: pi is not installed at ${piBinary}. Run \`bun install\` — pi is a devDependency and this guard judges the tree with the real loader.`
  }
  if (spawnSync('bun', ['--version'], { encoding: 'utf8' }).status !== 0) {
    return 'CANNOT CHECK: bun is not available, and the release packager this guard invokes is TypeScript that bun runs.'
  }
  const missing = PACKAGER_INPUTS.filter((input) => !existsSync(join(repoRoot, input)))
  if (missing.length > 0) {
    return `CANNOT CHECK: the packager's own inputs are missing from this checkout: ${missing.join(', ')}. Run \`bun install\` WITHOUT --ignore-scripts so the workspace dist trees are built.`
  }
  return undefined
}

/**
 * Stage an install-shaped tree with the release's OWN packager.
 *
 * `assembleVersionTree` is the one definition of what a release tree is —
 * `publishVersion` and `scripts/package-release.ts` are both callers. Invoking
 * it directly is what makes this guard's subject the shipped artifact rather
 * than a copy of the shipped artifact that can drift from it.
 *
 * The three staged "binaries" are one-byte stubs, and that is honest rather
 * than a shortcut: loading an extension never reads them, staging them needs
 * no cargo, and the packager's own zero-byte refusal still runs over them.
 */
function stageInstalledTree(into) {
  const stubs = join(into, 'stub-bin')
  mkdirSync(stubs, { recursive: true })
  const binaries = {}
  for (const name of ['chief', 'chiefd', 'beacond']) {
    const path = join(stubs, name)
    writeFileSync(path, '#!/bin/sh\nexit 0\n', { mode: 0o755 })
    binaries[name] = path
  }
  const tree = join(into, 'versions', '0.0.0-guard')
  mkdirSync(tree, { recursive: true })
  const driver = join(into, 'stage.ts')
  writeFileSync(
    driver,
    `import { assembleVersionTree } from ${JSON.stringify(releaseScript)}\n` +
      `const [treeDir, root, ...names] = process.argv.slice(2)\n` +
      `const binaries = Object.fromEntries(names.map((n) => [n.split('=')[0], n.split('=')[1]]))\n` +
      `assembleVersionTree(treeDir!, '0.0.0-guard', binaries, root!)\n`,
  )
  execFileSync(
    'bun',
    [driver, tree, repoRoot, ...Object.entries(binaries).map(([name, path]) => `${name}=${path}`)],
    { cwd: repoRoot, encoding: 'utf8', stdio: 'pipe' },
  )
  return join(tree, 'resources')
}

/** The shipped extensions that import a `@chief/*` package, read from the STAGED sources. */
function extensionsImportingChiefPackages(resources) {
  const dir = join(resources, 'packages', 'piing', 'extensions')
  return readdirSync(dir)
    .filter((entry) => entry.endsWith('.ts'))
    .map((entry) => ({ name: entry, path: join(dir, entry), source: readFileSync(join(dir, entry), 'utf8') }))
    .filter((extension) => /from\s+["']@chief\//.test(extension.source))
}

/**
 * Run the REAL pi against one staged extension and return everything it said.
 *
 * `--print` is what makes a pty unnecessary: pi's non-interactive mode runs to
 * completion on pipes. Extension loading happens before any model contact, so
 * `--offline` and an absent API key cost nothing this guard cares about.
 */
function loadExtensionWithPi(extensionPath, workspace) {
  const result = spawnSync(
    piBinary,
    [
      '--print',
      '--offline',
      '--no-session',
      '--no-context-files',
      '--no-skills',
      // ISOLATION, never a remedy — see this file's header.
      '--no-extensions',
      '--extension',
      extensionPath,
      'ping',
    ],
    {
      cwd: workspace,
      encoding: 'utf8',
      timeout: 90_000,
      env: {
        PATH: process.env.PATH,
        HOME: workspace,
        PI_OFFLINE: '1',
        // The five a managed pane always carries, so the extension gets past
        // its required-environment reads and reaches the pair check below.
        ORG_LAUNCHER_ORG_DIR: workspace,
        ORG_LAUNCHER_IDENTITY_DIR: workspace,
        ORG_LAUNCHER_ORGANIZATION: 'guard-company',
        ORG_LAUNCHER_ROOT: workspace,
        ORG_LAUNCHER_PERSON: 'guard-person',
        // SOCKET WITHOUT SESSION, deliberately: reaching the extension's own
        // pair check is proof that its module finished loading. See the
        // positive assertion.
        ORG_LAUNCHER_RUNTIME_SOCKET: 'chiefd-guard',
      },
    },
  )
  return `${result.stdout ?? ''}\n${result.stderr ?? ''}`
}

/**
 * A RESOLUTION failure, and deliberately not every load failure.
 *
 * Measured on this guard's own first CI run, which is why the distinction is
 * spelled out here rather than assumed: Pi wraps ANY throw from an extension's
 * activation in the same `Failed to load extension "<path>": …` sentence. So
 * matching that phrase conflates two opposite outcomes — a module that could
 * not be found, which is the outage, and a module that loaded, ran, and threw
 * on purpose, which is PROOF the outage is absent. The first version of this
 * guard matched the phrase and failed on its own positive signal.
 *
 * Only the module-resolution words are the defect: they are Node's, not Pi's
 * wrapper, and they are what the operator's dead panes carried.
 */
const RESOLUTION_FAILURE = /Cannot find module|ERR_MODULE_NOT_FOUND/

test('an installed release loads every @chief-importing extension in real pi', (t) => {
  const why = refusal()
  if (why) {
    t.diagnostic(why)
    return
  }
  const scratch = mkdtempSync(join(tmpdir(), 'chief-extension-load-'))
  try {
    const resources = stageInstalledTree(scratch)
    const workspace = join(scratch, 'workspace')
    mkdirSync(workspace, { recursive: true })
    const extensions = extensionsImportingChiefPackages(resources)
    assert.ok(
      extensions.length > 0,
      'no shipped extension imports a @chief package — either the tree did not stage, or this guard is reading the wrong directory',
    )

    const loaded = extensions.map((extension) => ({
      ...extension,
      output: loadExtensionWithPi(extension.path, workspace),
    }))
    for (const extension of loaded) {
      assert.doesNotMatch(
        extension.output,
        RESOLUTION_FAILURE,
        `pi could not resolve a module the shipped ${extension.name} imports, out of an installed tree. This is the outage: the release ships the runtime FILES without the package IDENTITY the extensions import them by.\n\npi said:\n${extension.output}`,
      )
    }

    // THE POSITIVE SIGNAL, and it is not optional.
    //
    // "no error appeared" passes vacuously the day pi rewords its failure. So
    // one run must be shown to have reached a line that only executes AFTER
    // module resolution finished — the intercom's own pair check, whose
    // sentence is read from the staged source rather than copied here, so a
    // reword breaks this loudly instead of silently.
    const intercom = loaded.find((extension) => extension.name === 'organization-intercom.ts')
    assert.ok(intercom, 'organization-intercom.ts must be among the shipped extensions')
    const pairSentence = intercom.source.match(
      /ORG_LAUNCHER_RUNTIME_SOCKET and ORG_LAUNCHER_RUNTIME_SESSION[^"'`]*/,
    )?.[0]
    assert.ok(pairSentence, "the intercom's pair-check sentence must still exist to be a positive signal")
    assert.match(
      intercom.output,
      new RegExp(pairSentence.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')),
      `the intercom never reached its own pair check, so this run proves nothing about module resolution.\n\npi said:\n${intercom.output}`,
    )
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
})

test('the control: with the package identity removed, pi fails exactly as the operator saw', (t) => {
  const why = refusal()
  if (why) {
    t.diagnostic(why)
    return
  }
  // THE RED-FIRST FIXTURE, REPRODUCED ON DEMAND.
  //
  // The packaging fix is merged, so the guard can no longer be shown failing
  // against the tree that shipped. This arm re-creates that tree exactly —
  // runtime files present, package identity absent, which is what the release
  // did — and asserts pi produces the operator's own error. It is what makes
  // the silence asserted above meaningful: an instrument that cannot be made
  // to go red has not been shown to see anything.
  const scratch = mkdtempSync(join(tmpdir(), 'chief-extension-load-control-'))
  try {
    const resources = stageInstalledTree(scratch)
    const workspace = join(scratch, 'workspace')
    mkdirSync(workspace, { recursive: true })
    const identities = join(resources, 'node_modules', '@chief')
    assert.ok(existsSync(identities), 'the packager must stage the package identities for this control to remove them')
    rmSync(identities, { recursive: true, force: true })

    const extensions = extensionsImportingChiefPackages(resources)
    const broken = extensions.map((extension) => ({
      name: extension.name,
      output: loadExtensionWithPi(extension.path, workspace),
    }))
    for (const { name, output } of broken) {
      assert.match(
        output,
        /Cannot find module '@chief\//,
        `removing the package identity must break ${name} with the operator's own error. It did not, so the passing arm above proves nothing.\n\npi said:\n${output}`,
      )
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
})

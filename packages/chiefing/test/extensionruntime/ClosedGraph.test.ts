import { readFileSync } from 'node:fs'

import {
  extensionRuntimeEntry,
  sourceRelativePath,
  staticModuleSpecifiers,
  walkExtensionRuntimeGraph
} from '@test/extensionruntime/Closure'
import { describe, expect, it } from 'vitest'

describe('extension-runtime closed graph', () => {
  const closure = walkExtensionRuntimeGraph()
  const relativePaths = closure.map(sourceRelativePath)

  it('walks the barrel and retains every verified transitive leaf', () => {
    // These imports are public dependencies (including postOrgRoute's shared
    // refusal decoder), so keep each as a non-vacuity anchor while discovering
    // the full graph from source rather than freezing another stale inventory.
    expect(relativePaths).toEqual(
      expect.arrayContaining([
        'extensionruntime/index.ts',
        'Nullish.ts',
        'resources/OrgRoutes.ts',
        'types/Health.ts',
        'types/Discovery.ts'
      ])
    )
  })

  // #983, and the non-vacuity anchor for the rule that changed below: an org
  // extension resolves its OWN company's daemon through beacond, so the
  // discovery client must actually reach a pi-home. Asserted positively as
  // well as by the absence of a ban, because a barrel export that silently
  // stopped being reachable would leave the ban's removal proving nothing.
  it('carries the discovery client, so an install can ask which daemon owns its company', () => {
    expect(relativePaths).toEqual(
      expect.arrayContaining(['discovery/Company.ts', 'discovery/DiscoveryClient.ts'])
    )
  })

  it('uses only copyable relative specifiers and node builtins', () => {
    for (const path of closure) {
      for (const specifier of staticModuleSpecifiers(path)) {
        expect(
          specifier.startsWith('.') || specifier.startsWith('node:'),
          `${sourceRelativePath(path)} imports forbidden ${specifier}`
        ).toBe(true)
      }
    }
  })

  // #751/P7 changed this test's subject once, deliberately, and #983 changed
  // it again for the same shape of reason.
  //
  // P7: it used to forbid `node:fs`, `AgentTokenManager` and
  // `resources/{Auth,Identity}.ts` outright, on the premise that a pane needs
  // no credentials — true only while chiefd authenticated an agent by walking
  // pid ancestry to the terminal pane it descended from. That walk is deleted,
  // so a pane MUST be able to read its own identity key and redeem it.
  //
  // #983: it also forbade `discovery/`, on the premise that a pane is handed
  // an already-resolved chiefd URL. That premise was the defect. The address
  // arrived as one process-global `ORG_CHIEFD_URL`, which has no single
  // correct value in a process hosting several companies — and the failure is
  // silent, because the wrong company's daemon answers rather than refusing.
  // An install now asks beacond which daemon owns ITS company, so the
  // discovery client belongs here. The ban is replaced by the positive
  // assertion above, never merely deleted.
  //
  // What survives BOTH changes, and is the part that actually keeps the copy
  // portable: no ambient environment reads (the discovery client takes its
  // beacond address as an argument and never touches `process.env`), and no
  // second crypto stack.
  it('reads no ambient environment and keeps third-party crypto out', () => {
    const forbiddenSource = /\bprocess\.env\b|jose/
    const forbiddenPaths = /^resources\/Auth\.ts$/

    for (const path of closure) {
      expect(readFileSync(path, 'utf8')).not.toMatch(forbiddenSource)
      expect(sourceRelativePath(path)).not.toMatch(forbiddenPaths)
    }
  })

  // The `node:fs` reach is sanctioned, but for TWO modules and two stated
  // purposes. Written as an allowlist rather than dropped: the reason the old
  // rule was a flat ban is that a copied extension graph reaching the
  // filesystem at all is worth knowing about, and that is still true of every
  // other module here.
  //
  //  - `resources/Identity.ts` reads this pane's own identity key.
  //  - `discovery/Rendezvous.ts` reads `<dir>/.chief/run/daemon.json`, which is
  //    how a pane learns where its own company's daemon is. That answer used to
  //    come from beacond over HTTP, keyed by slug — and a slug names no
  //    company, because two directories may hold companies with the same one.
  //    A pane's cwd IS its company directory, so the local read is both correct
  //    and cheaper; the file it reads is disposable runtime state, never
  //    authority.
  it('reaches the filesystem from exactly two modules, each for a stated reason', () => {
    const readers = closure
      .filter((path) => /node:fs/.test(readFileSync(path, 'utf8')))
      .map(sourceRelativePath)
      .sort()
    expect(readers).toEqual(['discovery/Rendezvous.ts', 'resources/Identity.ts'])
  })

  // A flat copy is what a pi-home receives, so two closure modules sharing a
  // basename would mean one silently overwriting the other in every agent's
  // home. `MaterializedCopy.test.ts` proves the copy itself; this proves the
  // property at the graph, where a new module is added.
  it('has no two modules whose basenames collide under the flat copy', () => {
    const basenames = relativePaths.map((path) => path.split('/').at(-1))
    expect(new Set(basenames).size).toBe(basenames.length)
  })

  // The lint config carries its OWN list of the files allowed to use relative
  // specifiers, because eslint cannot run the TypeScript walker above. That is
  // a second inventory of this graph, and a second inventory is exactly the
  // shape that goes stale — a module added to the closure whose lint exemption
  // is forgotten fails lint, which is loud; a module REMOVED from the closure
  // whose exemption survives is silent, and quietly licenses the next relative
  // import into a file the copy no longer carries. Checked here, where the
  // real graph is already computed.
  it('the lint exemption list is exactly this graph, in both directions', () => {
    const config = readFileSync(new URL('../../eslint.config.mjs', import.meta.url), 'utf8')
    const block = config.split("files: [\n      'src/extensionruntime/**/*.ts',")[1]
    expect(block, 'the extension-runtime lint exemption block moved or was renamed').toBeDefined()
    const listed = new Set(
      [...(block ?? '').split('],')[0].matchAll(/'(src\/[^']+)'/g)].map((match) =>
        (match[1] ?? '').replace(/^src\//, '')
      )
    )
    // The barrel itself is exempted by a glob, not by name.
    const walked = new Set(relativePaths.filter((path) => !path.startsWith('extensionruntime/')))
    expect([...listed].sort()).toEqual([...walked].sort())
  })

  it('keeps the barrel a view, never a second implementation', () => {
    const barrel = readFileSync(extensionRuntimeEntry, 'utf8')
    expect(barrel).not.toMatch(/^(export )?(class|function|const|let) /m)
  })
})

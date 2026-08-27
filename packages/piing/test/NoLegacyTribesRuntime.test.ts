import { existsSync } from 'node:fs'
import { join, resolve } from 'node:path'

import { expect, it } from 'vitest'

import { piingPackageRoot } from '@/runtime/PiPaths'

it('the generic launcher ships no legacy Tribes wallet, transaction, or login runtime', () => {
  const workspaceRoot = resolve(piingPackageRoot(), '..', '..')
  for (const relative of [
    'packages/piing/skills/wallet',
    'packages/piing/skills/transaction',
    'packages/piing/skills/tribes-login',
    'packages/piing/skills/prediction',
    'packages/piing/skills/spot-trading',
    'packages/piing/extensions/tribes',
    '.pi/prompts/tribes/login.md',
    'bootstrap.sh'
  ]) {
    expect(existsSync(join(workspaceRoot, relative)), relative).toBe(false)
  }
  // The second half asked `buildCatalog` whether any of those names could still
  // be DISCOVERED as a resource. There is no catalog (chief-home-is-cwd
  // §3/§4e) — Pi discovers skills itself from `<dir>/.pi/skills` — so absence
  // on disk is the whole claim, and it is the claim that matters: a file that
  // is not in the checkout cannot be loaded by anybody, catalog or not.
})

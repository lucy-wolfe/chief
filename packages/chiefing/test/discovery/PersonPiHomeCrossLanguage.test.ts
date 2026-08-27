/**
 * The cross-language pin for the GAP between what a pane is told and where its
 * own files are, in the same shape as `CompanyStorePathCrossLanguage.test.ts`
 * and for the same reason: a comment saying "must stay byte-identical" and a
 * test that checks it are different products, and only one of them has ever
 * caught anything.
 *
 * # The defect this exists for
 *
 * The commit that moved a company into the directory the operator stands in
 * left both TypeScript readers of a person's pi-home joining `people/` straight
 * onto `ORG_LAUNCHER_ORG_DIR` — the pane bearer acquirer
 * (`resources/PaneIdentity.ts`) and `organization-intercom`'s reload-hard
 * contract read. `chiefd-host` stamps that variable with `<dir>` EXACTLY while
 * materialization writes under `ActuatorConfig::data_root()`, which is
 * `<dir>/.chief`, so both readers looked one segment too shallow. Neither
 * FAILS on that: no key means "this pane has no credential, call token-less",
 * and no contract means "nothing changed". Nothing threw and nothing logged,
 * and under an enforced gate every org tool call from every pane was refused
 * with `missing bearer token`.
 *
 * # What is pinned HERE, and what is pinned by the live suites
 *
 * Here: the two halves of the gap, which are contracts rather than
 * implementation — the pane is stamped with the company DIRECTORY, and the
 * `.chief` root is that directory plus one segment. A TypeScript reader that
 * joins onto the stamp must close that gap itself, and `personPiHome` is the
 * one place it is closed.
 *
 * The trailing segments are pinned END TO END instead, by the four tool-contract
 * lanes (`packages/piing/test/toolcontract/*`): each boots a real `chiefd run`,
 * lets it materialize a real person, and then requires the pane transport to
 * find that person's key and mint a bearer. A behavioural proof against the
 * daemon that actually wrote the file cannot be satisfied by two sides
 * agreeing on a wrong answer, which a text match on the writer's own local
 * variables can — and those variables are the half of the writer most likely
 * to be rewritten for reasons that have nothing to do with this contract.
 */
import { readFileSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import { personPiHome } from '@/discovery/PersonPiHome'

function repoRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url))
  for (let depth = 0; depth < 10; depth += 1) {
    try {
      if (statSync(join(dir, 'apps', 'chiefd', 'crates')).isDirectory()) return dir
    } catch {
      // keep walking up
    }
    dir = dirname(dir)
  }
  throw new Error('could not locate the repo root (no apps/chiefd/crates above this test)')
}

const ROOT = repoRoot()

function read(relative: string): string {
  const source = readFileSync(join(ROOT, ...relative.split('/')), 'utf8')
  expect(source.length, `${relative} is empty — the pin would be vacuous`).toBeGreaterThan(0)
  return source
}

const CYCLE_RS = 'apps/chiefd/crates/chiefd-host/src/converge_apply/cycle.rs'

describe('what a pane is told, and where its own files are', () => {
  it('the TypeScript derivation produces <dir>/.chief/agent/<id>', () => {
    expect(personPiHome('/work/acme', 'ada')).toBe('/work/acme/.chief/agent/ada')
  })

  /**
   * HALF ONE OF THE GAP: the stamp is the company DIRECTORY.
   *
   * `cycle.rs` says so in as many words — "the pane variable
   * `ORG_LAUNCHER_ORG_DIR` is this value EXACTLY" — because it was once stamped
   * with the `.chief` root instead and every reader that joins onto it went one
   * level too deep. Stamping it back would make every TypeScript reader that
   * closes the gap itself wrong in the other direction, silently.
   */
  it('the pane is stamped with the company directory, not with the .chief root', () => {
    expect(read(CYCLE_RS)).toContain(
      'EnvAssignment::new("ORG_LAUNCHER_ORG_DIR", config.dir.display().to_string())'
    )
  })

  /**
   * HALF TWO: the root everything chief owns hangs off is that directory plus
   * exactly one segment. Together with the arm above, that is the whole gap a
   * TypeScript reader joining onto the stamp has to close — and `personPiHome`
   * is where this package closes it.
   */
  it('the .chief root is the stamped directory plus one segment, so a reader must close that gap', () => {
    const source = read(CYCLE_RS)
    expect(source).toContain('self.dir.join(CHIEF_DIR)')
    expect(source).toContain('const CHIEF_DIR: &str = ".chief";')
    expect(personPiHome('/work/acme', 'ada').startsWith('/work/acme/.chief/')).toBe(true)
  })

  it('negative self-check: a derivation that skipped the .chief root would not satisfy this pin', () => {
    // Proves the assertions above are real comparisons against real contents,
    // not vacuous truths about an empty read or a permissive substring.
    expect(personPiHome('/work/acme', 'ada')).not.toBe('/work/acme/people/ada/pi-home')
    expect(read(CYCLE_RS)).not.toContain(
      'EnvAssignment::new("ORG_LAUNCHER_ORG_DIR", config.data_root()'
    )
  })
})

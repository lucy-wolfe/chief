/**
 * #751/G12 — the one cross-language pin for a company's store path.
 *
 * The filename is derived independently in more than one language, and a
 * comment saying "must stay byte-identical" and a test that checks it are
 * different products; only one of them has ever caught anything.
 *
 * # What the pin covers now
 *
 *   1. `apps/chiefd/crates/chiefd-daemon/src/company_dir.rs` — `store_db_path`.
 *      The authoritative one: it is what `chiefd` actually opens.
 *   2. `packages/chiefing/src/discovery/CompanyStorePath.ts` —
 *      `companyStoreDbPath`, whose doc comment DEMANDS it stay byte-identical
 *      to the Rust derivation.
 *
 * There used to be a THIRD producer — `chiefd_store_db_path_for` in
 * `scripts/deploy/lib/deploy-common.sh`, in bash — but the go-public redaction
 * (E.2) deleted the whole private deploy tree, so the bash derivation is gone
 * and the Rust and TypeScript producers below are now the only two.
 *
 * `apps/chiefd/crates/chief-cli/src/paths.rs` carried a second Rust
 * derivation. Whether it survives the client-side rewrite is that packet's
 * call; if it does, it joins this pin.
 *
 * Why text and not execution: the Rust function is `pub(crate)`, so it cannot
 * be called from vitest. Reading the derivation is what is actually available,
 * and it is enough — a change to the folder names or the join order is exactly
 * the drift shape this guards, and all of it is visible in the text.
 */
import { readFileSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import { companyStoreDbPath } from '@/discovery/CompanyStorePath'

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

const COMPANY_DIR_RS = 'apps/chiefd/crates/chiefd-daemon/src/company_dir.rs'

describe('the company store path agrees across TypeScript and Rust (#751/G12)', () => {
  it('the TypeScript derivation produces <dir>/.chief/db/chief.db', () => {
    expect(companyStoreDbPath('/work/acme')).toBe('/work/acme/.chief/db/chief.db')
  })

  it('company_dir.rs joins the same three components in the same order', () => {
    const source = read(COMPANY_DIR_RS)
    expect(source).toContain('dir.join(".chief")')
    expect(source).toContain('chief_dir(dir).join("db").join("chief.db")')
  })

  /**
   * THE STORE IS INSIDE THE DIRECTORY IT DESCRIBES — the reversal this whole
   * layout turns on.
   *
   * The retired shape put it deliberately OUTSIDE, as
   * `<orgs>/.<slug>.chief.db`, so that `rm -rf <orgs>/<slug>/` could not
   * destroy the database before beacond's row. Removing a company is
   * `rm -rf <dir>/.chief` now, one act, and a store that survived it would be
   * the residue. Both halves must agree on the reversal, not just on the
   * string.
   */
  it('both derivations put the store INSIDE the company directory, not beside it', () => {
    expect(companyStoreDbPath('/work/acme').startsWith('/work/acme/')).toBe(true)
    expect(read(COMPANY_DIR_RS)).toContain(
      'assert!(store_db_path(dir).starts_with(dir), "the store is the directory\'s own file")'
    )
  })

  /**
   * TWO DIRECTORIES HOLDING SAME-NAMED COMPANIES GET TWO STORES.
   *
   * The case the slug-keyed filename could not represent: `.acme.chief.db`
   * under one orgs root was one file for both, and the second company
   * overwrote the first.
   */
  it('two same-named companies in different directories get different stores', () => {
    expect(companyStoreDbPath('/work/acme')).not.toBe(companyStoreDbPath('/elsewhere/acme'))
  })

  it('negative self-check: the text pins would notice a changed derivation', () => {
    // Proves the assertions above are real string comparisons against real
    // file contents, not vacuous truths about an empty read.
    const source = read(COMPANY_DIR_RS)
    expect(source).not.toContain('COMPANY_DB_SUFFIX')
    expect(source).not.toContain('.chief.db"')
  })
})

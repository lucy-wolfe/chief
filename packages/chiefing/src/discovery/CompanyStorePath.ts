import { join } from 'node:path'

/**
 * The SQLite file a company's chiefd owns: `<dir>/.chief/db/chief.db`.
 *
 * INSIDE the directory it describes, and slugless. The retired layout put it
 * deliberately OUTSIDE, as `<orgsRoot>/.<slug>.chief.db`, so that removing the
 * Pi-artifact tree `<orgsRoot>/<slug>/` could not destroy the database beside
 * it. That reason is gone with the tree it protected: a company IS a
 * directory, removing one is `rm -rf <dir>/.chief`, and a store that survived
 * that would be the residue.
 *
 * There is no slug validation left either, and nothing to validate: the slug
 * was a caller-supplied path segment that a `/` could walk out of the data
 * root with. Nothing caller-supplied reaches this join.
 *
 * Must stay byte-identical to the Rust derivation
 * (apps/chiefd/crates/chiefd-daemon/src/company_dir.rs::store_db_path).
 */
export function companyStoreDbPath(dir: string): string {
  return join(dir, '.chief', 'db', 'chief.db')
}

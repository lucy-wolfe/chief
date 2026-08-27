/**
 * A real company DIRECTORY with a real daemon rendezvous in it.
 *
 * # What this replaces, and why a fake is no longer needed
 *
 * An org extension used to learn where its company's daemon was by asking
 * beacond, so every suite here stood up a `FakeBeacond` — an HTTP server
 * implementing one read route — purely to answer that question. A company is a
 * DIRECTORY now, and its daemon publishes `<dir>/.chief/run/daemon.json` inside
 * it: the extension does one local read and consults no registry at all.
 *
 * So there is nothing left to fake. This writes the same four fields
 * `chiefd` writes (`host_primitives::rendezvous::DaemonRendezvous`), in a real
 * temp directory, and the production reader parses it for real.
 */
import { createHash } from 'node:crypto'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import type { CompanyDirectory } from '@test/types/CompanyRendezvous'

/** `sha256(<dir>)[..12]` — the identity `chiefd` derives from the one input it
 * is given, and publishes so nothing downstream has to derive it again. */
export function companyKeyFor(dir: string): string {
  return createHash('sha256').update(dir).digest('hex').slice(0, 12)
}

/** Where `chiefd` publishes it. Byte-identical to
 * `host_primitives::rendezvous::rendezvous_path`. */
export function rendezvousPathFor(dir: string): string {
  return join(dir, '.chief', 'run', 'daemon.json')
}

/**
 * Write one company directory's rendezvous, exactly as `chiefd` writes it.
 *
 * `recordedDir` defaults to `dir` and exists so a test can stage the
 * COPIED-PROJECT case: a rendezvous that names a directory it is not in must
 * be refused, never followed into the origin company's daemon.
 */
export function publishDaemonRendezvous(dir: string, url: string, recordedDir = dir): void {
  mkdirSync(join(dir, '.chief', 'run'), { recursive: true })
  /* eslint-disable lucy/no-json-stringify */
  // The wire body two programs decode. `toJsonTreeString` is private to a
  // sibling repo and is not a dependency anywhere in this workspace.
  writeFileSync(
    rendezvousPathFor(dir),
    JSON.stringify({ dir: recordedDir, key: companyKeyFor(recordedDir), url, pid: process.pid })
  )
  /* eslint-enable lucy/no-json-stringify */
}

/** Mint a company directory for `url`'s daemon and publish its rendezvous. */
export function createCompanyDirectory(url: string, prefix = 'piing-company-'): CompanyDirectory {
  const dir = mkdtempSync(join(tmpdir(), prefix))
  publishDaemonRendezvous(dir, url)
  return {
    dir,
    key: companyKeyFor(dir),
    remove: () => rmSync(dir, { recursive: true, force: true })
  }
}

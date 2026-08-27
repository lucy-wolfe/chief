/**
 * Public types for the shared `chiefd run --serve-only` vitest harness.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */

import type { AuthorizedFetch } from '@/types/OperatorBearer'

export interface CompanyDaemonOptions {
  /** The company's DISPLAY name — what `seedCompany` genesises it as. It names
   * nothing on the wire and nothing on disk: the daemon is told a directory
   * and resolves every route by `sha256(dir)[..12]`. */
  slug: string
  /** Repo root. Default: resolved upward from this module to the workspace root. */
  repoRoot?: string
  /** Temp-dir prefix. Default 'chief-company-test-'. */
  dirPrefix?: string
  /** Reachability deadline in ms. Default 20_000 — higher than the
   * docstore-only harness's, because a company daemon opens a company
   * database and mounts the full route surface, not just the schema. */
  readyTimeoutMs?: number
  /** Extra env for the child. Merged last. */
  env?: Readonly<Record<string, string>>
}

export interface CompanyDaemon {
  /** http://127.0.0.1:<port> */
  readonly url: string
  readonly port: number
  /** The company's DISPLAY name. Not an identity — two directories may hold
   * companies with the same one. */
  readonly slug: string
  /**
   * The key every `/v1/org/*` request body must carry as its `slug` —
   * `sha256(dir)[..12]`, which is what `CompanyDb::label()` holds.
   *
   * Exposed because getting it wrong is the single most common way to write a
   * test against this surface that fails with `404 unknown-company` and looks
   * like a routing bug. A caller building it by hand would be a second
   * implementation of a rule that has already gone wrong twice in this repo.
   */
  readonly companyKey: string
  /**
   * The company DIRECTORY this daemon was given as `--dir`. Everything it owns
   * hangs off `<dir>/.chief` — the store at `db/chief.db` and the
   * `keys/operator.key` this harness signs with.
   */
  readonly dir: string
  /**
   * The operator bearer this daemon minted for the harness, for a caller that
   * needs the raw token rather than [`CompanyDaemon.authorizedFetch`].
   */
  readonly bearer: string
  /**
   * `fetch` against this daemon carrying the operator bearer, with a single
   * re-acquisition on a `401`. Takes a PATH, not a URL — the origin is the
   * daemon's own.
   *
   * Every call a suite makes should go through this. A bare `fetch` at
   * `daemon.url` is anonymous and, once the universal gate is on, refused —
   * which is a thing a test may want to prove deliberately and never a thing
   * it should hit by accident.
   */
  readonly authorizedFetch: AuthorizedFetch
  /** stdout+stderr, for post-mortems. */
  readonly logPath: string
  readonly pid: number
  stop(): Promise<void>
}

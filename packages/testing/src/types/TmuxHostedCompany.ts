/**
 * Public types for the tmux-hosted `chiefd run` harness.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */

/** Everything the `chiefd run` command line for one hosted company depends on. */
export interface ChiefdRunArgvOptions {
  /** The company DIRECTORY this daemon serves (`--dir`). Its whole
   * configuration: the store, the keys and the wire identity all hang off it. */
  readonly dir: string
  /** The private tmux socket this boot actuates on (`--runtime-socket`). */
  readonly tmuxSocket: string
  /**
   * The checkout under test, pinned as `--launcher-root`. Never omitted: see
   * `chiefdRunArgv`'s own doc for the shared-home fallback this exists to
   * keep the harness out of.
   */
  readonly repoRoot: string
}

export interface TmuxHostedCompanyOptions {
  /** The company slug this daemon serves. */
  slug: string
  /** Repo root. Default: resolved upward from this module to the workspace root. */
  repoRoot?: string
  /** Temp-dir prefix. Default 'chief-tmux-host-test-'. */
  dirPrefix?: string
  /**
   * Reachability deadline in ms. Default 30_000 — higher than the
   * `--serve-only` harness's, because this boot chain is three processes
   * (beacond, the company-row write, then the daemon's own port walk and
   * beacond admission) rather than one.
   */
  readyTimeoutMs?: number
  /** Extra env for the chiefd child. Merged last. */
  env?: Readonly<Record<string, string>>
}

export interface TmuxHostedCompany {
  /** http://127.0.0.1:<port> — the company daemon's own bound address. */
  readonly url: string
  readonly port: number
  /** The company's DISPLAY name. Not an identity — two directories may hold
   * companies with the same one. */
  readonly slug: string
  /**
   * The key every `/v1/org/*` request body must carry as its `slug` —
   * `sha256(dir)[..12]`. A display slug answers `unknown-company` on every
   * route.
   */
  readonly companyKey: string
  /**
   * The company DIRECTORY: the daemon's `--dir`, the beacond row's primary
   * key, and what an agent's `ORG_LAUNCHER_ORG_DIR` names. Everything chief
   * owns for this company is under `<dir>/.chief`.
   */
  readonly dir: string
  /** The private tmux socket this daemon actuates on. */
  readonly tmuxSocket: string
  /** http://127.0.0.1:<port> — the test-owned beacond this daemon registered with. */
  readonly beacondUrl: string
  /** chiefd stdout+stderr, for post-mortems. */
  readonly logPath: string
  /** beacond stdout+stderr, for post-mortems. */
  readonly beacondLogPath: string
  readonly pid: number
  stop(): Promise<void>
}

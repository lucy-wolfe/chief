/**
 * Public types for the harness's operator-bearer acquirer.
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 */

export interface OperatorBearerOptions {
  /** The daemon's bare origin, e.g. `http://127.0.0.1:41234`. */
  url: string
  /**
   * The folder that HOLDS `keys/operator.key` — `<dir>/.chief` for a company
   * daemon. Named for what it contains rather than for a root: its predecessor
   * was called `dataRoot` and shared that name with the orgs root a daemon was
   * given as `--data-root`, and confusing the two cost this repo a day (#13).
   */
  keysRoot: string
  /** Defaults to `operator`, the id `enroll_bootstrap_operator` hardcodes. */
  identityId?: string
}

/**
 * A `fetch` that carries an operator bearer, minting one lazily on the first
 * call and re-acquiring exactly once on a `401`.
 *
 * Same recovery the pane transport and the Rust operator client both
 * implement, and for the same reason: chiefd's HS256 signing secret rotates on
 * restart unless a secret file was provisioned, so a long-lived caller's token
 * dies without anybody re-execing.
 */
export type AuthorizedFetch = (
  path: string,
  init?: { method?: string; headers?: Record<string, string>; body?: string }
) => Promise<Response>

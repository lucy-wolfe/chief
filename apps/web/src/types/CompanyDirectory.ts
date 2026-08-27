/** Public types for the company directory.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** One company: that it exists, and whether its daemon answers.
 *
 * `status` is the PROBE's answer, not the registry's. A row keeps its url
 * after a daemon dies without deregistering, so a registered url is not proof
 * of life — treating it as proof is how a directory shows "running" for a
 * company whose every request then 502s. */
export interface ChiefdHealth {
  readonly healthy: boolean
  readonly httpStatus?: number
  readonly reason?: string
}

export interface CompanyDirectoryEntry {
  /** `sha256(dir)[..12]` — the company's identity, and the handle every route
   * in this app addresses it by. Served on the beacond row; never derived
   * here. */
  readonly key: string
  /** The directory the company occupies. Its `.chief/keys/operator.key` is the
   * credential this server signs with. */
  readonly dir: string
  /** The company's DISPLAY name. Not an identity — two directories may hold
   * companies with the same one, and the directory lists both. */
  readonly slug: string
  readonly status: 'running' | 'stopped'
  readonly url?: string
  readonly chiefd: ChiefdHealth
}

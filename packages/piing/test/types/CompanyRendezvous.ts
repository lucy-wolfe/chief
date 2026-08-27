/** Public types for the company-rendezvous fixture.
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** A disposable company directory with a daemon already published in it. */
export interface CompanyDirectory {
  /** The directory itself — what a pane's `ORG_LAUNCHER_ORG_DIR` names. */
  readonly dir: string
  /** `sha256(dir)[..12]`, the `slug` every chiefd route resolves by. */
  readonly key: string
  remove(): void
}

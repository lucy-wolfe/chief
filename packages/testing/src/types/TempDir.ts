/** A temp tree a test owns, and the idempotent removal that ends it. */
export interface TempDir {
  readonly path: string
  remove(): Promise<void>
}

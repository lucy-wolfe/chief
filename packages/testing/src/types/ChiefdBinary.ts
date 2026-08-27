/**
 * Public types for `@/ChiefdBinary`. Housed here per
 * `lucy/no-exported-type-outside-types-dir`.
 */

/** The result of `chiefdBinaryTestGate` (#846): whether a test suite gated
 * on the real chiefd binary should run for real, or has already been
 * established safe to skip. CI's own absence case never reaches this — the
 * gate throws before returning in that case. */
export interface ChiefdBinaryTestGate {
  readonly present: boolean
  readonly binaryPath: string
}

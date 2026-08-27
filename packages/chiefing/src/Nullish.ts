/**
 * Centralized null/undefined check, per `lucy/no-raw-null-check`. This repo
 * has no `@tribes-terminal/foundation` (the package the rule's terminal
 * origin names) — see `packages/eslinter/rules/no-raw-null-check.js`'s
 * `allowedPaths` option (E9-S1/#833, ruling on #842) for the mechanism that
 * lets this file be the one place raw comparisons are legal. This exact
 * exemption is declared in `packages/chiefing/eslint.config.mjs`, mirroring
 * the testing package's `src/Nullish.ts`.
 */
export function isNullish(value: unknown): value is null | undefined {
  return value === null || value === undefined
}

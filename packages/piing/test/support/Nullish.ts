/**
 * Local test-only `isNullish`. `@chief/chiefing` and `@chief/testing` both
 * define an equivalent helper (`src/Nullish.ts`), but neither re-exports it
 * from its public barrel — importing a package-internal path is not a valid
 * cross-package dependency here. Ported suites that previously wrote raw
 * `=== undefined`/`!== null` checks route through this instead.
 */
export function isNullish(value: unknown): value is null | undefined {
  return value === null || value === undefined
}

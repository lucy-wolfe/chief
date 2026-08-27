/**
 * The one null/undefined test in apps/web.
 *
 * `lucy/no-raw-null-check` forbids `x === null` / `x === undefined` at call
 * sites: the two are distinct values and a raw check almost always means one
 * of them, while the caller meant "absent". One predicate keeps that decision
 * in a single place.
 */
export function isNullish(value: unknown): value is null | undefined {
  /* eslint-disable lucy/no-raw-null-check */
  // THE one raw check. The rule has to be satisfied somewhere, and this module
  // exists to be that somewhere — disabling it here is what lets every call
  // site keep the rule.
  return value === null || value === undefined
  /* eslint-enable lucy/no-raw-null-check */
}

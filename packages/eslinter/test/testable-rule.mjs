// One seam, one explanation, ten call sites.
//
// Every rule in this package is built by `@typescript-eslint/utils`'s
// `ESLintUtils.RuleCreator` (see `../utils.js`), which returns a
// `RuleModuleWithName<...>`. Every rule TEST drives it through `RuleTester`
// imported from `eslint`, whose `run` types its rule argument as
// `@eslint/core`'s `RuleDefinition<RuleDefinitionTypeOptions>`. Those are the
// same object at runtime — `RuleTester` executes these rules correctly, and
// has for the life of this package — declared by two packages that do not
// know about each other's declaration. There is no runtime question here to
// answer, only a declaration mismatch to state once.
//
// It is stated ONCE, here, rather than as ten copies of the same cast with
// ten copies of the same comment. The cast target is DERIVED from `run`'s own
// signature rather than written out, so it cannot drift from whatever
// `RuleTester` actually accepts after an eslint upgrade: if the parameter
// type changes, this changes with it.
//
// This is deliberately NOT exported from `../utils.js`. `knip.jsonc` ignores
// `packages/*/test/**`, so an export in `utils.js` used only by tests would
// be reported as dead and either deleted or excused with an ignore row — the
// exact failure knip.jsonc's own header comment describes.

import { RuleTester } from 'eslint'

/**
 * @param {unknown} rule a rule module produced by `createRule`
 * @returns {Parameters<RuleTester['run']>[1]}
 */
export function testableRule(rule) {
  return /** @type {Parameters<RuleTester['run']>[1]} */ (rule)
}

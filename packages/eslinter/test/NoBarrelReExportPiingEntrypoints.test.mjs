import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-barrel-re-export.js'

import { testableRule } from './testable-rule.mjs'

// Regression lock for the packages/piing barrel entrypoints added to
// no-barrel-re-export.js's allowlist by E0-S5 (#756). The allowlist this rule
// ported from terminal only ever carried terminal-repo paths; without an
// entry for this chief workspace package's own barrel, `packages/piing`
// could never ship the `src/index.ts` / `src/extensionruntime/index.ts`
// re-export barrels its Contract (the E3 epic, #786) requires. This test
// asserts both are sanctioned AND that a non-entrypoint file under the same
// package still gets flagged — the allowlist targets exact files, not the
// whole package.
RuleTester.describe = describe
RuleTester.it = it

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module'
  }
})

ruleTester.run('no-barrel-re-export (packages/piing entrypoints)', testableRule(rule), {
  valid: [
    {
      filename: '/repo/packages/piing/src/index.ts',
      code: "export { piingSkillsRoot } from './runtime/PiPaths'"
    },
    {
      filename: '/repo/packages/piing/src/extensionruntime/index.ts',
      code: "export { GOAL_PRIORITIES } from './GoalPriority'"
    }
  ],
  invalid: [
    {
      filename: '/repo/packages/piing/src/runtime/PiPaths.ts',
      code: "export { piingSkillsRoot } from './PiPaths'",
      errors: [{ messageId: 'noBarrelReExport' }]
    },
    {
      filename: '/repo/packages/piing/src/extensionruntime/GoalPriority.ts',
      code: "export * from './ReloadSentinel'",
      errors: [{ messageId: 'noBarrelReExport' }]
    }
  ]
})

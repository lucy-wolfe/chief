import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-raw-null-check.js'

import { testableRule } from './testable-rule.mjs'

// Regression lock for the `allowedPaths` option added to no-raw-null-check.js
// by E9-S1 (#833, ruling on #842). The rule was ported from terminal
// pointing engineers at `isNullish()` from `@tribes-terminal/foundation` — a
// package this repo does not have and will not gain. Rather than hardcode a
// chief-specific exemption (a Mandate-0 stopgap), the rule gained a
// `allowedPaths` option mirroring `no-process-env`'s exact shape and
// substring-match semantics, so any package's own local isNullish()
// implementation can declare itself. Both halves matter: a path IN
// allowedPaths is exempt, and a path NOT in it is still flagged — the
// negative half is what stops the option from becoming a blanket disable.
RuleTester.describe = describe
RuleTester.it = it

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module'
  }
})

ruleTester.run('no-raw-null-check (allowedPaths option)', testableRule(rule), {
  valid: [
    {
      filename: '/repo/packages/testing/src/Nullish.ts',
      code: "export function isNullish(value) { return value === null || value === undefined }",
      options: [{ allowedPaths: ['/packages/testing/src/Nullish.ts'] }]
    },
    // the two hardcoded terminal-origin isNullish() implementations still
    // exempt themselves with no option at all
    {
      filename: '/repo/packages/foundation/src/utils/lang.ts',
      code: 'export function isNullish(value) { return value === null }'
    }
  ],
  invalid: [
    {
      code: 'if (x === null) { y() }',
      options: [{ allowedPaths: ['/packages/testing/src/Nullish.ts'] }],
      errors: [{ messageId: 'useIsNullish' }]
    },
    {
      filename: '/repo/packages/other/src/Something.ts',
      code: 'if (x === undefined) { y() }',
      errors: [{ messageId: 'useIsNullish' }]
    }
  ]
})

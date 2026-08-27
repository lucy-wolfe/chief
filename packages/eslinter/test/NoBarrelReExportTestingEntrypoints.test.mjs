import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-barrel-re-export.js'

import { testableRule } from './testable-rule.mjs'

// Regression lock for the packages/testing barrel entrypoint added to
// no-barrel-re-export.js's allowlist by E9-S1 (#833). Same shape as the
// packages/piing lock (E0-S5/#756): the allowlist this rule ported from
// terminal only ever carried terminal-repo paths, so without an entry for
// this chief workspace package's own barrel, `packages/testing` could never
// ship the `src/index.ts` re-export barrel its Contract (E9 epic, #839)
// requires. Asserts the barrel is sanctioned AND that a non-entrypoint file
// under the same package still gets flagged — the allowlist targets exact
// files, not the whole package.
RuleTester.describe = describe
RuleTester.it = it

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module'
  }
})

ruleTester.run('no-barrel-re-export (packages/testing entrypoint)', testableRule(rule), {
  valid: [
    {
      filename: '/repo/packages/testing/src/index.ts',
      code: "export { startCompanyDaemon } from './CompanyDaemon'"
    }
  ],
  invalid: [
    {
      filename: '/repo/packages/testing/src/CompanyDaemon.ts',
      code: "export { allocateEphemeralPort } from './EphemeralPort'",
      errors: [{ messageId: 'noBarrelReExport' }]
    },
    {
      filename: '/repo/packages/testing/src/types/TempDataRoot.ts',
      code: "export * from './Provided'",
      errors: [{ messageId: 'noBarrelReExport' }]
    }
  ]
})

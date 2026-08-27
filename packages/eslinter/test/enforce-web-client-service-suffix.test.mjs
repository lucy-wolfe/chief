import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/enforce-web-client-service-suffix.js'

import { testableRule } from './testable-rule.mjs'

RuleTester.describe = describe
RuleTester.it = it

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module'
  }
})

// Regression lock for E6-S1 (#806) restoring this rule from E0-S3's (#754)
// DROPPED_RULES list — it was misclassified as terminal-domain when chief had
// no apps/web yet, but the rule is gated only on the literal path
// `/apps/web/src/` (see RuleCatalog.test.mjs). Both halves matter for a
// path-gated rule: a correctly-named ClientService file/class must stay
// clean, a bare Service file/class under apps/web must be flagged, AND the
// same bare-Service shape OUTSIDE apps/web/src/ must be untouched — that
// boundary is the entire rule.
ruleTester.run('enforce-web-client-service-suffix', testableRule(rule), {
  valid: [
    {
      code: 'export class CompaniesClientService {}',
      filename: '/repo/apps/web/src/services/CompaniesClientService.ts'
    },
    {
      // The same bare "Service" shape is untouched outside apps/web/src/ —
      // this is the boundary the rule actually draws.
      code: 'export class CompaniesService {}',
      filename: '/repo/apps/api/src/services/CompaniesService.ts'
    }
  ],
  invalid: [
    {
      code: 'export const x = 1',
      filename: '/repo/apps/web/src/services/CompaniesService.ts',
      errors: [{ messageId: 'useClientServiceFilename' }]
    },
    {
      code: 'export class CompaniesService {}',
      filename: '/repo/apps/web/src/services/CompaniesClientService.ts',
      errors: [{ messageId: 'useClientServiceClass' }]
    }
  ]
})

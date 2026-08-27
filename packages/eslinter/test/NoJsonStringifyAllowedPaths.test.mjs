import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-json-stringify.js'

import { testableRule } from './testable-rule.mjs'

// Regression lock for the `allowedPaths` option added to no-json-stringify.js
// (E9-S1/#833, ruling on #842, second instance found independently by
// eng-e2-s1 while filling packages/chiefing within the same hour). Same
// defect class as no-raw-null-check: the rule was ported from terminal
// pointing engineers at `toJsonTreeString`/`ensureJsonTreeString` from
// `@tribes-terminal/foundation`, a package this repo does not have and will
// not gain. `allowedPaths` mirrors `no-process-env`'s exact shape and
// substring-match semantics. Both halves matter: a path IN allowedPaths may
// call JSON.stringify(), and a path NOT in it is still flagged — the
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

ruleTester.run('no-json-stringify (allowedPaths option)', testableRule(rule), {
  valid: [
    {
      filename: '/repo/packages/chiefing/src/transport/JsonTreeString.ts',
      code: 'export function toJsonTreeString(value) { return JSON.stringify(value) }',
      options: [{ allowedPaths: ['/packages/chiefing/src/transport/JsonTreeString.ts'] }]
    }
  ],
  invalid: [
    {
      filename: '/repo/packages/chiefing/src/transport/FetchTransport.ts',
      code: 'const body = JSON.stringify(payload)',
      options: [{ allowedPaths: ['/packages/chiefing/src/transport/JsonTreeString.ts'] }],
      errors: [{ messageId: 'useToJsonTreeString' }]
    },
    {
      filename: '/repo/packages/other/src/Something.ts',
      code: 'const body = JSON.stringify(payload)',
      errors: [{ messageId: 'useToJsonTreeString' }]
    }
  ]
})

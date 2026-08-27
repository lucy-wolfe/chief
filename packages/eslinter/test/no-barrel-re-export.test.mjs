import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-barrel-re-export.js'

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

// Regression lock for the E0-S4 (#755) allowlist entry: @chief/chiefing's
// single barrel is sanctioned, but the allowlist targets that ONE file, not
// the whole package — a non-entrypoint file under packages/chiefing must
// still be flagged, or the entry is a hole rather than a door.
ruleTester.run('no-barrel-re-export', testableRule(rule), {
  valid: [
    {
      code: "export { ChiefdClient } from './ChiefdClient'",
      filename: '/repo/packages/chiefing/src/index.ts'
    },
    {
      code: "export type { ChiefdClientOptions } from './types/Transport'",
      filename: '/repo/packages/chiefing/src/index.ts'
    },
    {
      code: "export { DocsClient } from '../resources/Docs'",
      filename: '/repo/packages/chiefing/src/extensionruntime/index.ts'
    }
  ],
  invalid: [
    {
      code: "export { DocsClient } from './resources/Docs'",
      filename: '/repo/packages/chiefing/src/resources/Manifest.ts',
      errors: [{ messageId: 'noBarrelReExport' }]
    },
    {
      // A file that merely ENDS with "index.ts" elsewhere in the package
      // (not the package root's src/index.ts) must still be flagged — the
      // allowlist matches the exact barrel path, not any index.ts.
      code: "export { Foo } from './Foo'",
      filename: '/repo/packages/chiefing/src/resources/index.ts',
      errors: [{ messageId: 'noBarrelReExport' }]
    },
    {
      code: "export { DocsClient } from '../resources/Docs'",
      filename: '/repo/packages/chiefing/src/extensionruntime/not-an-entrypoint.ts',
      errors: [{ messageId: 'noBarrelReExport' }]
    }
  ]
})

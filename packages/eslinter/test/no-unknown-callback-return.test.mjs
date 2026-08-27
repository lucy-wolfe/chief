import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-unknown-callback-return.js'

import { testableRule } from './testable-rule.mjs'

RuleTester.describe = describe
RuleTester.it = it

const here = dirname(fileURLToPath(import.meta.url))

// Syntax-only rule -- no type information needed, but the shared parser
// config is still required to parse TS interface/type-literal syntax.
const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module',
    parserOptions: {
      tsconfigRootDir: here
    }
  }
})

ruleTester.run('no-unknown-callback-return', testableRule(rule), {
  valid: [
    // The two sanctioned narrowings this rule exists to require.
    'interface Runtime { reconcile: (slug: string) => void; }',
    'interface Runtime { reconcile: (slug: string) => void | Promise<void>; }',
    'interface Runtime { reconcile: (slug: string) => void | Promise<unknown>; }',
    // A concrete return type carries real information; not the laundering
    // shape this rule targets.
    'interface Runtime { reconcile: (slug: string) => boolean; }',
    // #866's own confirmed-benign case: an opaque timer HANDLE that is
    // consumed downstream (by clearInterval), not a discarded callback
    // return -- the rule can't see that distinction syntactically, so it's
    // the documented false-positive escape hatch. The disable comment must
    // be INSIDE the linted code string (RuleTester lints exactly this
    // string, not the surrounding test file), and must use RuleTester's
    // internal `rule-to-test/` prefix rather than the real `lucy/` prefix
    // apps/cli/src/legacy/organization/org-log.ts's own disable comment uses.
    [
      '// eslint-disable-next-line rule-to-test/no-unknown-callback-return -- opaque handle, consumed by clearInterval, not a discarded promise',
      'interface Heartbeat { setInterval?: (handler: () => void, ms: number) => unknown; }'
    ].join('\n'),
    // A method (not property) signature with a real return type.
    'interface Runtime { reconcile(slug: string): boolean; }',
    // A plain function declaration is not a callback-seam field.
    'function reconcile(slug: string): unknown { return undefined }',
    // An `unknown` PARAMETER (not return type) is unrelated to this rule.
    'interface Runtime { reconcile: (payload: unknown) => void; }'
  ],
  invalid: [
    {
      code: 'interface Runtime { reconcile: (slug: string) => unknown; }',
      errors: [{ messageId: 'narrowReturnType', data: { keyword: 'unknown' } }]
    },
    {
      code: 'interface Runtime { reconcile: (slug: string) => any; }',
      errors: [{ messageId: 'narrowReturnType', data: { keyword: 'any' } }]
    },
    // Method-signature shorthand, not just the arrow-typed property shape.
    {
      code: 'interface Runtime { reconcile(slug: string): unknown; }',
      errors: [{ messageId: 'narrowReturnType', data: { keyword: 'unknown' } }]
    },
    // A standalone function-type alias used as a callback seam.
    {
      code: 'type Reconciler = (slug: string) => unknown;',
      errors: [{ messageId: 'narrowReturnType', data: { keyword: 'unknown' } }]
    },
    // Inline object type literal, not only a named interface.
    {
      code: 'function f(opts: { reconcile: (slug: string) => unknown }) {}',
      errors: [{ messageId: 'narrowReturnType', data: { keyword: 'unknown' } }]
    }
  ]
})

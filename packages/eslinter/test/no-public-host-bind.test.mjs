import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-public-host-bind.js'

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

ruleTester.run('no-public-host-bind', testableRule(rule), {
  valid: [
    // env-derived host constant — the sanctioned pattern
    'server.listen(PORT, HTTP_HOST, onListen)',
    'Bun.serve({ hostname: HTTP_HOST, port: PORT, fetch: handler })',
    // explicit loopback binds
    "server.listen(PORT, '127.0.0.1')",
    "server.listen(PORT, 'localhost')",
    "Bun.serve({ hostname: '::1', port: 3000 })",
    // no host argument at all
    'server.listen(PORT)',
    'server.listen({ port: PORT })',
    // unix domain sockets
    "server.listen('/run/sandboxd.sock')",
    'Bun.serve({ unix: socketPath, fetch: handler })',
    // public literal outside a bind call (e.g. the env.ts conditional)
    "const HTTP_HOST = IS_PRODUCTION ? '0.0.0.0' : '127.0.0.1'",
    // unrelated call with a matching-looking string
    "logger.info('listening', { host: '0.0.0.0' })"
  ],
  invalid: [
    {
      code: "server.listen(PORT, '0.0.0.0', onListen)",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "server.listen(PORT, '::')",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "server.listen({ port: PORT, host: '0.0.0.0' })",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "Bun.serve({ hostname: '0.0.0.0', port: 3000, fetch: handler })",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "Bun.serve({ hostname: '[::]', port: 3000 })",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "serve({ hostname: '0.0.0.0', port: 3000 })",
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: 'server.listen(PORT, `0.0.0.0`)',
      errors: [{ messageId: 'noPublicHostBind' }]
    },
    {
      code: "new WebSocketServer({ host: '0.0.0.0', port: 8080 })",
      errors: [{ messageId: 'noPublicHostBind' }]
    }
  ]
})

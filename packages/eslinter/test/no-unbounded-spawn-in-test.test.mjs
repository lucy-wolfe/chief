import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { describe, it } from 'vitest'

import rule from '../rules/no-unbounded-spawn-in-test.js'

import { testableRule } from './testable-rule.mjs'

RuleTester.describe = describe
RuleTester.it = it

const here = dirname(fileURLToPath(import.meta.url))

// Syntax-only rule -- no type information needed.
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

ruleTester.run('no-unbounded-spawn-in-test', testableRule(rule), {
  valid: [
    // Spawns nothing -- the rule must not fire on an ordinary test.
    "import { describe, it } from 'vitest'\nit('adds', () => { expect(1 + 1).toBe(2) })",

    // BLOCKING: execFileSync with its own `timeout` option.
    [
      "import { execFileSync } from 'node:child_process'",
      "import { it } from 'vitest'",
      "it('runs', () => { execFileSync('bun', ['x'], { cwd: '.', timeout: 30_000 }) })"
    ].join('\n'),

    // BLOCKING: Bun.spawnSync with its own `timeout` option, called from a
    // helper OUTSIDE the test -- the blocking check is purely local to the
    // call, so nesting doesn't matter.
    [
      "import { it } from 'vitest'",
      "function run() { return Bun.spawnSync(['bun'], { timeout: 30_000 }) }",
      "it('runs', () => { run() })"
    ].join('\n'),

    // ASYNC: spawn() directly inside a timed it() (number third arg).
    [
      "import { spawn } from 'node:child_process'",
      "import { it } from 'vitest'",
      "it('runs', async () => { spawn('bun', ['x']) }, 30_000)"
    ].join('\n'),

    // ASYNC: Bun.spawn directly inside a timed it() ({ timeout } object).
    [
      "import { it } from 'vitest'",
      "it('runs', async () => { Bun.spawn(['bun']) }, { timeout: 30_000 })"
    ].join('\n'),

    // ASYNC: new RpcClient(...) directly inside a timed it() (named
    // constant third arg -- trusted at face value, matches the real
    // MainDispatch.test.ts convention).
    [
      "import { RpcClient } from '@earendil-works/pi-coding-agent'",
      "import { it } from 'vitest'",
      "const SPAWN_TEST_TIMEOUT_MS = 30_000",
      "it('runs', async () => { new RpcClient({}) }, SPAWN_TEST_TIMEOUT_MS)"
    ].join('\n'),

    // ASYNC via one level of named-function indirection -- the exact
    // FakeRpcChild.test.ts shape: the spawn lives in a helper function
    // defined outside any it(), called from a timed it().
    [
      "import { RpcClient } from '@earendil-works/pi-coding-agent'",
      "import { it } from 'vitest'",
      "function clientWithScript() { return new RpcClient({}) }",
      "it('runs', async () => { clientWithScript() }, 30_000)"
    ].join('\n'),

    // Same indirection shape via a `const name = () => ...` helper.
    [
      "import { RpcClient } from '@earendil-works/pi-coding-agent'",
      "import { it } from 'vitest'",
      "const makeClient = () => new RpcClient({})",
      "it('runs', async () => { makeClient() }, 30_000)"
    ].join('\n'),

    // The reviewed known-spawning-FUNCTION registry (not `new`
    // construction) -- startCompanyDaemon, directly inside a timed it().
    [
      "import { startCompanyDaemon } from '@chief/testing'",
      "import { it } from 'vitest'",
      "it('runs', async () => { startCompanyDaemon({}) }, 20_000)"
    ].join('\n'),

    // A same-named function from an UNREGISTERED source is not a spawn --
    // the registry is source-scoped, not a name guess.
    [
      "import { startCompanyDaemon } from './totally-unrelated-helper'",
      "import { it } from 'vitest'",
      "it('runs', () => { startCompanyDaemon({}) })"
    ].join('\n'),

    // EXPORTED factory, never called from this file at all (the real
    // NodeSpawnPort.ts shape -- its two callers live in other test files
    // entirely). Cross-file tracing is out of reach; an exported
    // function's callers are unknown, not flagged.
    [
      "import { spawn } from 'node:child_process'",
      "export function nodeSpawnPort() { return spawn('node', []) }"
    ].join('\n'),

    // EXPORTED `const` factory form of the same exemption.
    [
      "import { spawn } from 'node:child_process'",
      "export const nodeSpawnPort = () => spawn('node', [])"
    ].join('\n')
  ],
  invalid: [
    // BLOCKING: execFileSync with no `timeout` option.
    {
      code: [
        "import { execFileSync } from 'node:child_process'",
        "import { it } from 'vitest'",
        "it('runs', () => { execFileSync('bun', ['x'], { cwd: '.' }) })"
      ].join('\n'),
      errors: [{ messageId: 'blockingNeedsOwnTimeout', data: { name: 'execFileSync' } }]
    },
    // BLOCKING: Bun.spawnSync with no options object at all.
    {
      code: [
        "import { it } from 'vitest'",
        "it('runs', () => { Bun.spawnSync(['bun']) })"
      ].join('\n'),
      errors: [{ messageId: 'blockingNeedsOwnTimeout', data: { name: 'Bun.spawnSync' } }]
    },
    // Known-spawning-function registry: the company daemon (A6(c)).
    // `startCompanyDaemon` spawns `chiefd run --serve-only`, so an UNTIMED
    // it() that boots it is the defect this rule exists for.
    {
      code: [
        "import { startCompanyDaemon } from '@chief/testing'",
        "import { it } from 'vitest'",
        "it('runs', async () => { startCompanyDaemon({}) })"
      ].join('\n'),
      errors: [{ messageId: 'asyncNeedsTestTimeout', data: { name: 'startCompanyDaemon' } }]
    },
    // The same registry entry reached through packages/testing's own internal
    // alias -- the entry the package's OWN suites import through, so it must
    // be registered separately from the public barrel.
    {
      code: [
        "import { startCompanyDaemon } from '@/CompanyDaemon'",
        "import { it } from 'vitest'",
        "it('runs', async () => { startCompanyDaemon({}) })"
      ].join('\n'),
      errors: [{ messageId: 'asyncNeedsTestTimeout', data: { name: 'startCompanyDaemon' } }]
    },
    // ASYNC: spawn() directly inside an UNTIMED it().
    {
      code: [
        "import { spawn } from 'node:child_process'",
        "import { it } from 'vitest'",
        "it('runs', async () => { spawn('bun', ['x']) })"
      ].join('\n'),
      errors: [{ messageId: 'asyncNeedsTestTimeout', data: { name: 'spawn' } }]
    },
    // ASYNC: new RpcClient(...) directly inside an UNTIMED it().
    {
      code: [
        "import { RpcClient } from '@earendil-works/pi-coding-agent'",
        "import { it } from 'vitest'",
        "it('runs', async () => { new RpcClient({}) })"
      ].join('\n'),
      errors: [{ messageId: 'asyncNeedsTestTimeout', data: { name: 'RpcClient' } }]
    },
    // ASYNC via named-function indirection, but the ONLY call site is an
    // untimed it() -- the indirection must not launder a genuinely
    // unbounded spawn.
    {
      code: [
        "import { RpcClient } from '@earendil-works/pi-coding-agent'",
        "import { it } from 'vitest'",
        "function clientWithScript() { return new RpcClient({}) }",
        "it('runs', async () => { clientWithScript() })"
      ].join('\n'),
      errors: [{ messageId: 'asyncOutsideTest', data: { name: 'RpcClient' } }]
    },
    // ASYNC via named-function indirection where the helper is never
    // called from any it()/test() the rule can see at all.
    {
      code: [
        "import { RpcClient } from '@earendil-works/pi-coding-agent'",
        "function clientWithScript() { return new RpcClient({}) }"
      ].join('\n'),
      errors: [{ messageId: 'asyncOutsideTest', data: { name: 'RpcClient' } }]
    }
  ]
})

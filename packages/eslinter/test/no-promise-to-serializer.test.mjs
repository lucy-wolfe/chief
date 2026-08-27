import { dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

import tsParser from '@typescript-eslint/parser'
import { RuleTester } from 'eslint'
import { beforeAll, describe, expect, it } from 'vitest'

import rule from '../rules/no-promise-to-serializer.js'

import { testableRule } from './testable-rule.mjs'

RuleTester.describe = describe
RuleTester.it = it

const here = dirname(fileURLToPath(import.meta.url))

// `projectService.allowDefaultProject` intentionally permits only eight
// synthetic files by default. Keep one distinct client file for each typed
// RuleTester fixture: the service treats a filename as mutable source state,
// so sharing RuleTester's implicit `file.ts` let stale source state cross
// otherwise independent fixture boundaries in CI.
const DEFAULT_PROJECT_FILE_CAP = 8
const fixtureFiles = Object.freeze({
  awaited: `${here}/awaited.ts`,
  ordinaryObject: `${here}/ordinary-object.ts`,
  handledPromise: `${here}/handled-promise.ts`,
  otherSerializer: `${here}/other-serializer.ts`,
  directPromise: `${here}/direct-promise.ts`,
  maybeAsync: `${here}/maybe-async.ts`,
  serializerArguments: `${here}/serializer-arguments.ts`,
  configuredSerializer: `${here}/configured-serializer.ts`
})
const defaultProjectFixtureFiles = Object.freeze(Object.values(fixtureFiles))
const defaultProjectFixtureNames = Object.freeze(
  defaultProjectFixtureFiles.map((filename) => filename.slice(`${here}/`.length))
)
// This real source file belongs to its own tiny configured project. Warming
// it initializes the parser's project service without opening or mutating one
// of the eight default-project client files used by RuleTester below.
const warmupRelativeFile = 'project-service-warmup/warmup.ts'
const warmupFile = `${here}/${warmupRelativeFile}`

// Type-aware testing needs a real TypeScript program behind each fixture.
// `projectService` + `allowDefaultProject` builds one in-memory client file
// per fixture rather than requiring a real tsconfig.json + on-disk files for
// every case (typescript-eslint's sanctioned way to type-check RuleTester
// fixtures that don't correspond to real files) — the SAME mechanism
// `no-bignumber-to-string`'s type check depends on, just not yet exercised
// by a test in this package before this rule.
const parserOptions = {
  projectService: {
    allowDefaultProject: defaultProjectFixtureNames,
    // Do not silently expand the parser's default-project budget when a new
    // fixture is added. This is deliberately the library's normal cap.
    maximumDefaultProjectFileMatchCount_THIS_WILL_SLOW_DOWN_LINTING: DEFAULT_PROJECT_FILE_CAP
  },
  tsconfigRootDir: here
}

const ruleTester = new RuleTester({
  languageOptions: {
    parser: tsParser,
    ecmaVersion: 2021,
    sourceType: 'module',
    parserOptions
  }
})

// The project service's expensive host initialization is first triggered by
// parsing a file (measured: the FIRST typed case pays ~1-5s depending on
// machine contention, then subsequent cases run in tens of milliseconds).
// Left on a behavioral fixture, that one-time cold start raced vitest's
// default 5000ms per-test budget — a resource-availability cost masquerading
// as an assertion timeout. The parser has no public "just warm the service"
// API narrower than parsing a file, so this hook parses a real file in a
// dedicated configured project. It moves setup cost out of the behavioral
// assertions without putting mutable warmup source state in any of their
// default-project client files.
//
// 20s here is not "raise the timeout until it passes" — that move is
// explicitly rejected for the actual assertions below, which keep vitest's
// default. This is a ONE-TIME setup cost, isolated to a hook that asserts
// nothing about behaviour, sized against measured worst-case cold-start
// under heavy oversubscription (~5s at 5x this suite's own core count),
// not against a quiet default that will not survive a busier machine.
beforeAll(async () => {
  // `projectService`'s object form is real and honoured by the parser, but
  // `parseForESLint`'s declared `ParserOptions` does not carry it.
  await tsParser.parseForESLint('export const projectServiceWarmup = 1\n', {
    .../** @type {Record<string, unknown>} */ (parserOptions),
    // This path is included by project-service-warmup/tsconfig.json, not by
    // allowDefaultProject, so the warmup cannot leave mutable source state in
    // any RuleTester fixture.
    filePath: warmupFile
  })
}, 20_000)

const fixtures = {
  valid: [
    // The sanctioned fix: await first.
    {
      filename: fixtureFiles.awaited,
      code: 'declare function doSomethingAsync(x: string): Promise<string>;\nasync function f(x: string) { JSON.stringify(await doSomethingAsync(x)) }'
    },
    // A non-Promise argument is fine.
    {
      filename: fixtureFiles.ordinaryObject,
      code: 'JSON.stringify({ a: 1 })'
    },
    // A Promise used somewhere OTHER than a serializer argument is not
    // this rule's concern (no-floating-promises already covers a bare
    // floating call; this rule is specifically about the serializer case).
    {
      filename: fixtureFiles.handledPromise,
      code: 'declare function doSomethingAsync(x: string): Promise<string>;\nasync function f(x: string) { const p = doSomethingAsync(x); await p }'
    },
    // A configured serializer that is NOT called still leaves other
    // JSON.stringify-shaped calls alone if the object name differs.
    {
      filename: fixtureFiles.otherSerializer,
      code: "declare function doSomethingAsync(x: string): Promise<string>;\nasync function f(x: string) { const NotJSON = { stringify: (v: unknown) => String(v) }; NotJSON.stringify(doSomethingAsync(x)) }"
    }
  ],
  invalid: [
    {
      filename: fixtureFiles.directPromise,
      code: 'declare function doSomethingAsync(x: string): Promise<string>;\nfunction f(x: string) { JSON.stringify(doSomethingAsync(x)) }',
      errors: [{ messageId: 'awaitBeforeSerializing' }]
    },
    // A Promise buried in a union (e.g. an incompletely-narrowed
    // conditional) is still a Promise on at least one branch.
    {
      filename: fixtureFiles.maybeAsync,
      code: 'declare function maybeAsync(x: string): Promise<string> | string;\nfunction f(x: string) { JSON.stringify(maybeAsync(x)) }',
      errors: [{ messageId: 'awaitBeforeSerializing' }]
    },
    // A second argument position is checked too, not just the first.
    {
      filename: fixtureFiles.serializerArguments,
      code: 'declare function doSomethingAsync(x: string): Promise<string>;\nfunction f(x: string) { JSON.stringify({ a: 1 }, undefined); JSON.stringify(doSomethingAsync(x), null, 2) }',
      errors: [{ messageId: 'awaitBeforeSerializing' }]
    },
    // A project-configured additional serializer (the future
    // toJsonTreeString/ensureJsonTreeString case, proven with a stand-in
    // name here since neither helper exists in this repo yet).
    {
      filename: fixtureFiles.configuredSerializer,
      code: 'declare function toJsonTreeString(v: unknown): string;\ndeclare function doSomethingAsync(x: string): Promise<string>;\nfunction f(x: string) { toJsonTreeString(doSomethingAsync(x)) }',
      options: [{ serializers: [{ method: 'toJsonTreeString' }] }],
      errors: [{ messageId: 'awaitBeforeSerializing' }]
    }
  ]
}

it('keeps typed RuleTester fixtures within the default-project client-file cap without aliasing warmup', () => {
  const filenames = [...fixtures.valid, ...fixtures.invalid].map(({ filename }) => filename)

  expect(filenames).toHaveLength(DEFAULT_PROJECT_FILE_CAP)
  expect(new Set(filenames).size).toBe(DEFAULT_PROJECT_FILE_CAP)
  expect(filenames).toEqual(defaultProjectFixtureFiles)
  expect(defaultProjectFixtureNames).toHaveLength(DEFAULT_PROJECT_FILE_CAP)
  expect(defaultProjectFixtureNames).not.toContain(warmupRelativeFile)
  expect(filenames).not.toContain(warmupFile)
})

ruleTester.run('no-promise-to-serializer', testableRule(rule), {
  valid: fixtures.valid,
  invalid: fixtures.invalid
})

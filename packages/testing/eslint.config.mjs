import { FlatCompat } from '@eslint/eslintrc'
import js from '@eslint/js'
import typescriptEslint from '@typescript-eslint/eslint-plugin'
import typescriptParser from '@typescript-eslint/parser'
import importPlugin from 'eslint-plugin-import'
import jasminePlugin from 'eslint-plugin-jasmine'
import prettierPlugin from 'eslint-plugin-prettier'
import simpleImportSortPlugin from 'eslint-plugin-simple-import-sort'
import unusedImportsPlugin from 'eslint-plugin-unused-imports'
import { dirname } from 'path'
import { fileURLToPath } from 'url'
import lucyPlugin from '../../packages/eslinter/index.js'
import {
  createExactPackageJsonDependencyVersionsConfig,
  sharedIgnores,
  sharedTypeScriptRules
} from '../../eslint.config.mjs'

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)

const compat = new FlatCompat({
  baseDirectory: __dirname
})

const eslintConfig = [
  js.configs.recommended,
  ...compat.extends('prettier'),
  {
    files: ['**/*.ts'],
    languageOptions: {
      parser: typescriptParser,
      ecmaVersion: 2021,
      sourceType: 'module',
      globals: {
        console: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
        setInterval: 'readonly',
        clearInterval: 'readonly',
        process: 'readonly',
        Buffer: 'readonly',
        fetch: 'readonly'
      },
      parserOptions: {
        project: ['./tsconfig.build.json', './tsconfig.vitest.json'],
        tsconfigRootDir: __dirname
      }
    },
    plugins: {
      '@typescript-eslint': typescriptEslint,
      import: importPlugin,
      jasmine: jasminePlugin,
      prettier: prettierPlugin,
      'simple-import-sort': simpleImportSortPlugin,
      'unused-imports': unusedImportsPlugin,
      lucy: lucyPlugin
    },
    rules: {
      ...sharedTypeScriptRules,
      'import/no-extraneous-dependencies': [
        'error',
        {
          packageDir: ['.', '../../'],
          devDependencies: true
        }
      ],
      'lucy/no-json-stringify': 'error',
      'lucy/no-promise-to-serializer': 'error',
      // This repo has no @tribes-terminal/foundation isNullish() — this
      // package's own Nullish.ts (@/Nullish) is the local implementation the
      // one call site that genuinely needs it (CompanyDaemon.ts's
      // exitCode/signalCode check, where a truthy rewrite would treat a
      // clean exit-0 as "still running") imports from. Ruling on #842.
      'lucy/no-raw-null-check': ['error', { allowedPaths: ['/packages/testing/src/Nullish.ts'] }],
      'lucy/no-optional-nullable': 'error',
      'lucy/no-pass-through-alias-export': 'error',
      'lucy/no-barrel-re-export': 'error',
      'lucy/no-generic-filenames': 'error',
      'lucy/no-console-usage': 'error',
      'lucy/no-dead-address-literal': 'error',
      'lucy/no-indexed-type-access': 'error',
      'lucy/no-inline-zod-infer': 'error',
      'lucy/no-async-in-utils': 'error',
      // Unlike every other package so far, this harness's whole job is
      // spawning a real chiefd subprocess and managing its environment:
      // honoring CARGO_TARGET_DIR and building the child's env. There is no
      // apps/*/src/common/env.ts to centralize into — this package is not an
      // app. `allowedPaths` is the rule's own built-in escape hatch for
      // exactly this case. Covers test/ too: the self-tests exercise the
      // same env plumbing directly (CARGO_TARGET_DIR), and one of them
      // asserts the global setup writes NOTHING into the ambient
      // environment, which it can only do by reading it.
      'lucy/no-process-env': ['error', { allowedPaths: ['/packages/testing/'] }],
      'lucy/no-default-in-enum-switch': 'error',
      'lucy/prefer-switch-for-enum': 'error',
      'lucy/no-raw-zod-bigint': 'error',
      'lucy/require-eslint-disable-explanation': 'error',
      'lucy/enforce-url-constructor-two-args': 'error',
      'lucy/no-exported-type-outside-types-dir': 'error',
      'lucy/no-empty-file': 'error',
      'lucy/enforce-test-file-location': 'error',
      'lucy/enforce-test-import-alias': 'error',
      'lucy/no-v8-ignore': 'error'
    }
  },
  {
    // #855: `lucy/no-unbounded-spawn-in-test` fires on `it()`/`test()`-
    // relative shapes that only exist inside test files -- scoped here,
    // not the main `**/*.ts` block above, matching apps/cli/apps/api's own
    // scoping (registering it package-wide there produced a real false
    // positive on production code that spawns outside any test at all).
    // This package's own daemon-booting suites were the real gap that
    // motivated wiring this rule here at all -- none of them had a per-test
    // deadline before this story.
    files: ['test/**/*.ts'],
    languageOptions: {
      parser: typescriptParser,
      ecmaVersion: 2021,
      sourceType: 'module',
      globals: {
        console: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
        setInterval: 'readonly',
        clearInterval: 'readonly',
        process: 'readonly',
        Buffer: 'readonly',
        fetch: 'readonly'
      },
      parserOptions: {
        project: ['./tsconfig.build.json', './tsconfig.vitest.json'],
        tsconfigRootDir: __dirname
      }
    },
    plugins: {
      lucy: lucyPlugin
    },
    rules: {
      'lucy/no-unbounded-spawn-in-test': 'error'
    }
  },
  // `ReactiveScan.test.ts` drives `scripts/reactive-scan.ts` — the real
  // repo-wide scanner behind `bun run lint:reactive` — and its allowlist. Those
  // files sit at the repository root, outside every package's `@/*` alias, so a
  // relative import is the only way to reach the subject at all; the
  // alternative is a test that re-derives the scanner's behavior from source
  // text, which is precisely the kind of test that keeps passing after the
  // scanner goes blind. Scoped to this one file and this one rule, on the
  // packages/piing precedent — never a widening for ordinary `src/**` imports.
  // It lived in `apps/cli/test/` until P3 deleted that package.
  {
    files: ['test/ReactiveScan.test.ts'],
    rules: {
      '@typescript-eslint/no-restricted-imports': 'off'
    }
  },
  // `ReleaseArtifact.test.ts` drives `scripts/release-chiefd.ts` and
  // `scripts/package-release.ts` — the real release-artifact emitters behind
  // `bun run release` and the CI packaging step. Same reason as
  // `ReactiveScan.test.ts` above: those files sit at the repository root,
  // outside every package's `@/*` alias, so a relative import is the only way
  // to reach the subject; re-deriving the emitters from source text is exactly
  // the test that keeps passing after they drift. Scoped to this one file and
  // this one rule.
  {
    files: ['test/ReleaseArtifact.test.ts'],
    rules: {
      '@typescript-eslint/no-restricted-imports': 'off'
    }
  },
  createExactPackageJsonDependencyVersionsConfig(lucyPlugin),
  {
    ignores: sharedIgnores
  }
]

export default eslintConfig

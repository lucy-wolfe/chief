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
      'lucy/no-raw-null-check': [
        'error',
        {
          // This package's own isNullish() implementation — see Nullish.ts's
          // doc comment (mirrors @chief/testing/src/Nullish.ts's exemption).
          allowedPaths: ['packages/chiefing/src/Nullish.ts']
        }
      ],
      'lucy/no-optional-nullable': 'error',
      'lucy/no-pass-through-alias-export': 'error',
      'lucy/no-barrel-re-export': 'error',
      'lucy/no-generic-filenames': 'error',
      'lucy/no-console-usage': 'error',
      'lucy/no-dead-address-literal': 'error',
      'lucy/no-indexed-type-access': 'error',
      'lucy/no-inline-zod-infer': 'error',
      'lucy/no-async-in-utils': 'error',
      'lucy/no-process-env': 'error',
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
    // #775 (E2-S6): `src/sse/**` is republished verbatim into pi-homes by
    // E2-S8 (`chiefing/extension-runtime`), which have no `node_modules` —
    // panes cannot resolve the `@/*` path alias or any package specifier
    // there. These files therefore import ONLY relative siblings (and node/
    // web builtins), which is exactly what `no-restricted-imports`'s
    // `patterns: ['../*', './*']` (above) forbids for every other file in
    // this package. Scoped off here rather than weakened package-wide — the
    // constraint is real for this one directory, not a general exemption.
    files: ['src/sse/**/*.ts'],
    rules: {
      '@typescript-eslint/no-restricted-imports': 'off'
    }
  },
  {
    // E2-S8's extension-runtime graph is copied into a Pi home with no
    // node_modules and no tsconfig paths mapping. Every reachable module must
    // therefore use real relative specifiers (or node: builtins), including
    // the transitive helpers outside src/extensionruntime itself. Keep this
    // exception to that graph alone: normal chiefing code still uses @/.
    files: [
      'src/extensionruntime/**/*.ts',
      'src/Errors.ts',
      'src/Nullish.ts',
      'src/transport/FetchFailure.ts',
      'src/transport/FetchTransport.ts',
      'src/transport/RetryPolicy.ts',
      'src/resources/Docs.ts',
      'src/resources/RowStores.ts',
      'src/resources/OrgRoutes.ts',
      'src/resources/FounderLaunch.ts',
      'src/sse/SseFrames.ts',
      'src/sse/SseWatcher.ts',
      'src/sse/SseHub.ts',
      'src/types/Transport.ts',
      'src/types/Watch.ts',
      'src/types/OrgDocs.ts',
      'src/types/RowDocs.ts',
      'src/types/Health.ts',
      'src/types/Discovery.ts',
      // #751/P7: a pane authenticates with the identity key in its own
      // pi-home, so the token manager and the key reader joined the copied
      // graph. `resources/Auth.ts` (enrolment, an operator concern) did NOT —
      // it would collide with `types/Auth.ts` in the flat copy.
      'src/resources/AgentToken.ts',
      'src/resources/Identity.ts',
      'src/types/Auth.ts',
      // A4: the ONE pane-side acquirer. `team-ui` and every SSE reader ran in
      // the same pane over the same key and reached chiefd with no credential
      // — not for want of a key, but because the acquirer lived inside one
      // extension. It is shared runtime now, so it joins the copied graph.
      'src/resources/PaneIdentity.ts',
      // #983: an org extension resolves ITS OWN company's daemon instead of
      // reading a process-global address, so company discovery is part of the
      // copied graph now. `ClosedGraph.test.ts` asserts this list against the
      // graph the walker actually discovers, so it cannot drift into a stale
      // second inventory of the closure.
      //
      // `Rendezvous.ts` is how a pane answers that question today: its cwd IS
      // its company directory, so it reads `<dir>/.chief/run/daemon.json` and
      // consults no registry at all. `DiscoveryClient.ts` stays in the graph
      // for the box-wide question a pane's boot ladder can still ask.
      'src/discovery/Company.ts',
      'src/discovery/DiscoveryClient.ts',
      'src/discovery/Rendezvous.ts',
      // The company-directory layout a pane joins onto its own stamp. A pane is
      // told `<dir>` and everything chief owns lives under `<dir>/.chief`, so
      // the segment between them is one derivation rather than one per reader
      // — and both readers that need it, the bearer acquirer here and
      // `organization-intercom`, run inside the copied graph.
      'src/discovery/PersonPiHome.ts'
    ],
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

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

const assetRuntimeRules = {
  '@typescript-eslint/no-floating-promises': 'error',
  '@typescript-eslint/no-misused-promises': 'error',
  '@typescript-eslint/await-thenable': 'error',
  'lucy/no-promise-to-serializer': 'error',
  'lucy/no-unknown-callback-return': 'error'
}

const eslintConfig = [
  // #785 moved the Pi runtime asset trees into this package. They retain
  // their runtime-specific style and import conventions, so they receive the
  // deliberately narrow type-aware policy below rather than the ordinary
  // package-source policy. Scope js/recommended away as well: its plain-JS
  // globals do not understand the asset runtime's TypeScript context.
  { ...js.configs.recommended, ignores: ['extensions/**', 'skills/**'] },
  ...compat.extends('prettier'),
  {
    files: ['**/*.ts'],
    // Keep the full package lint policy on authored package source, tests,
    // and scripts. The moved runtime assets have a separate, explicit
    // type-aware configuration below; they are not ignored globally.
    ignores: ['extensions/**', 'skills/**'],
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
        fetch: 'readonly',
        // Ambient TypeScript namespace, not a runtime value — needed so
        // `no-undef` (a plain-JS rule with no type awareness) does not flag
        // type-only references like `NodeJS.Signals`/`NodeJS.Timeout` in
        // test fixtures that type real `child_process`/timer handles.
        NodeJS: 'readonly'
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
        // Matches @chief/chiefing's and @chief/testing's own eslint.config.mjs
        // overrides for their equivalent Nullish.ts: neither re-exports
        // isNullish from its public barrel, so ported test suites route
        // through this package-local implementation instead.
        { allowedPaths: ['/packages/piing/test/support/Nullish.ts'] }
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
  createExactPackageJsonDependencyVersionsConfig(lucyPlugin),
  // Pi runtime assets are copied into Pi homes and predate the package's
  // authored-source formatting, alias, and API conventions. They must still
  // have real parser-project coverage: a missing project boundary used to
  // report one opaque parser error per moved source and hid every safety
  // diagnostic behind it. The two guarded root projects own these exact
  // files, so reuse them rather than invent a permissive broad config.
  //
  // This mirrors apps/cli's `src/legacy/**` policy: keep a small type-aware
  // set that finds discarded/misused promises and promise serialization, but
  // do not turn a mechanical relocation into a mass style rewrite. The full
  // package rule set above continues to govern `src/**`, `test/**`, and
  // `scripts/**` unchanged.
  {
    files: ['extensions/**/*.ts'],
    languageOptions: {
      parser: typescriptParser,
      ecmaVersion: 2021,
      sourceType: 'module',
      parserOptions: {
        project: ['../../tsconfig.extensions.json'],
        tsconfigRootDir: __dirname
      }
    },
    plugins: {
      '@typescript-eslint': typescriptEslint,
      lucy: lucyPlugin
    },
    rules: assetRuntimeRules
  },
  {
    ignores: sharedIgnores
  },
  // `src/extensionruntime/**` is not ordinary package-internal code: its
  // Contract (E3 epic #786, "@chief/piing/extension-runtime" subpath)
  // requires the whole directory to stay a SELF-CONTAINED closure that a
  // materializer copies byte-for-byte as sibling files into a pi-home
  // extension directory outside this package (and outside any bundler) —
  // node builtins + type-only pi imports only, wired together with plain
  // relative `./sibling` specifiers, because that is the only import form
  // that still resolves once the files are copied elsewhere. Two shared
  // rules assume ordinary package layout and must be relaxed only here:
  //   - `@typescript-eslint/no-restricted-imports` (via
  //     sharedTypeScriptRules) bans './'/'../' imports repo-wide in favor of
  //     the `@/*` alias — but `@/*` does not exist once these files are
  //     copied out of this package, so the closure needs real relative
  //     imports to stay self-contained after materialization.
  //   - `lucy/no-exported-type-outside-types-dir` would otherwise force
  //     `GoalPriority`/`FocusOrderItem` into `src/types/`, which is exactly
  //     the cross-directory import materialization cannot follow.
  {
    files: ['src/extensionruntime/**/*.ts'],
    rules: {
      '@typescript-eslint/no-restricted-imports': 'off',
      'lucy/no-exported-type-outside-types-dir': 'off'
    }
  },
  // E4-S8 (#794): a handful of `test/**` regression files import real,
  // exported symbols straight out of `extensions/**` (e.g.
  // `PaneEndpoint`, `ClassifierMigration`) to assert on the actual production classes/
  // functions rather than re-deriving their behavior from source text. Those
  // files sit outside the `@/*`-aliased `src/**` tree entirely (same reason
  // `extensions/**` itself is relative-only, above), so a relative import is
  // the only way to reach them — never a widening of the restriction for
  // ordinary `src/**` imports elsewhere in `test/**`.
  {
    files: [
      'test/PaneEndpoint.test.ts',
      'test/ClassifierMigration.test.ts',
      'test/DepartmentScopeDenial.test.ts',
      'test/ExecutiveRootIsNotExempt.test.ts',
      'test/RosterAuthorityVisibility.test.ts',
      'test/extensions/BusEventsBoundedAppend.test.ts',
      'test/extensions/FounderLaunchProgress.test.ts',
      'test/extensions/IdentityKeyRefusalIsReported.test.ts',
      'test/extensions/OrgSendReplayWindow.test.ts',
      'test/extensions/PaneCallersPresentABearer.test.ts'
    ],
    rules: {
      '@typescript-eslint/no-restricted-imports': 'off'
    }
  }
]

export default eslintConfig

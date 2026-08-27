import { FlatCompat } from '@eslint/eslintrc'
import js from '@eslint/js'
import typescriptEslint from '@typescript-eslint/eslint-plugin'
import typescriptParser from '@typescript-eslint/parser'
import importPlugin from 'eslint-plugin-import'
import jasminePlugin from 'eslint-plugin-jasmine'
import prettierPlugin from 'eslint-plugin-prettier'
import reactPlugin from 'eslint-plugin-react'
import reactHooksPlugin from 'eslint-plugin-react-hooks'
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
    files: ['**/*.ts', '**/*.tsx', '**/*.js', '**/*.jsx'],
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
        global: 'readonly',
        window: 'readonly',
        document: 'readonly',
        navigator: 'readonly',
        fetch: 'readonly',
        localStorage: 'readonly',
        sessionStorage: 'readonly'
      },
      parserOptions: {
        // tsconfig.json excludes test/ (Next apps' generated shape, per the
        // terminal precedent this story copies) — tsconfig.vitest.json is
        // the one that includes it, so both must be listed here or every
        // test file fails to parse (mirrors packages/chiefing's eslint.config.mjs).
        project: ['./tsconfig.json', './tsconfig.vitest.json'],
        tsconfigRootDir: __dirname,
        ecmaFeatures: {
          jsx: true
        }
      }
    },
    plugins: {
      '@typescript-eslint': typescriptEslint,
      react: reactPlugin,
      'react-hooks': reactHooksPlugin,
      import: importPlugin,
      jasmine: jasminePlugin,
      prettier: prettierPlugin,
      'simple-import-sort': simpleImportSortPlugin,
      'unused-imports': unusedImportsPlugin,
      lucy: lucyPlugin
    },
    rules: {
      ...sharedTypeScriptRules,
      'lucy/no-json-stringify': 'error',
      'lucy/no-promise-to-serializer': 'error',
      'lucy/no-raw-null-check': 'error',
      'lucy/no-optional-nullable': 'error',
      'lucy/no-pass-through-alias-export': 'error',
      'lucy/no-barrel-re-export': 'error',
      'lucy/no-generic-filenames': 'error',
      'lucy/enforce-web-client-service-suffix': 'error',
      'lucy/no-inline-zod-infer': 'error',
      'lucy/no-console-usage': 'error',
      'lucy/no-dead-address-literal': 'error',
      'lucy/no-indexed-type-access': 'error',
      'lucy/no-async-in-utils': 'error',
      'lucy/no-process-env': ['error', { allowedPaths: ['/apps/web/src/common/'] }],
      'lucy/no-default-in-enum-switch': 'error',
      'lucy/prefer-switch-for-enum': 'error',
      'lucy/no-raw-zod-bigint': 'error',
      'lucy/require-eslint-disable-explanation': 'error',
      'lucy/enforce-url-constructor-two-args': 'error',
      'lucy/no-exported-type-outside-types-dir': 'error',
      'lucy/no-empty-file': 'error',
      'lucy/no-v8-ignore': 'error',
      'react/react-in-jsx-scope': 'off',
      'react/prop-types': 'off',
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
      '@typescript-eslint/no-restricted-imports': [
        'error',
        {
          patterns: ['../*', './*']
        }
      ]
    },
    settings: {
      react: {
        version: 'detect'
      }
    }
  },
  {
    files: ['next.config.ts'],
    rules: {
      'lucy/no-process-env': 'off'
    }
  },
  createExactPackageJsonDependencyVersionsConfig(lucyPlugin),
  {
    ignores: [...sharedIgnores]
  }
]

export default eslintConfig

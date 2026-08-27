import prettierConfig from './prettier.config.mjs'
import * as jsoncEslintParserModule from 'jsonc-eslint-parser'

const jsoncEslintParser =
  'default' in jsoncEslintParserModule
    ? jsoncEslintParserModule.default
    : jsoncEslintParserModule

// Base TypeScript ESLint rules that are shared across all projects
export const sharedTypeScriptRules = {
  // Import rules
  'import/no-extraneous-dependencies': [
    'error',
    {
      packageDir: ['.', '../../'],
      devDependencies: true
    }
  ],
  'import/no-duplicates': 'error',
  'simple-import-sort/imports': 'error',
  'simple-import-sort/exports': 'error',
  'unused-imports/no-unused-imports': 'error',

  // TypeScript rules
  '@typescript-eslint/await-thenable': 'error',
  // `paths` held one entry, banning `zeroAddress` from `viem`. It went with
  // the dependency: `viem` is no longer installed, so the ban had nothing
  // left to ban, and a restriction with no subject is dead config.
  '@typescript-eslint/no-restricted-imports': ['error', { patterns: ['../*', './*'] }],
  '@typescript-eslint/switch-exhaustiveness-check': 'error',
  '@typescript-eslint/no-floating-promises': 'error',
  '@typescript-eslint/no-inferrable-types': [
    'error',
    {
      ignoreParameters: true,
      ignoreProperties: true
    }
  ],
  '@typescript-eslint/no-explicit-any': 'error',
  '@typescript-eslint/explicit-function-return-type': [
    'error',
    {
      allowExpressions: true,
      allowTypedFunctionExpressions: true,
      allowHigherOrderFunctions: true,
      allowDirectConstAssertionInArrowFunctions: true,
      allowConciseArrowFunctionExpressionsStartingWithVoid: true
    }
  ],
  '@typescript-eslint/explicit-module-boundary-types': 'off',
  '@typescript-eslint/no-unused-vars': [
    'error',
    { varsIgnorePattern: '^_', argsIgnorePattern: '^_' }
  ],
  '@typescript-eslint/no-useless-constructor': ['error'],
  '@typescript-eslint/no-non-null-assertion': 'error',
  '@typescript-eslint/no-unsafe-member-access': 'error',
  '@typescript-eslint/consistent-type-assertions': [
    'error',
    {
      assertionStyle: 'never'
    }
  ],

  // General code quality rules
  'no-unused-vars': 'off', // Use @typescript-eslint/no-unused-vars instead
  'no-unused-private-class-members': 'off', // Use @typescript-eslint version instead
  '@typescript-eslint/no-unused-private-class-members': 'error',
  'no-restricted-properties': [
    'error',
    {
      object: 'Sentry',
      property: 'captureException',
      message:
        'Avoid calling Sentry.captureException directly. Report errors through logger.error(...) instead.'
    }
  ],
  'no-void': 'off',
  'max-len': [
    'error',
    {
      code: 100,
      ignoreStrings: true
    }
  ],
  eqeqeq: ['error', 'always'],
  radix: ['error', 'as-needed'],
  'object-shorthand': ['error', 'always'],
  'no-useless-constructor': 'off',
  'no-async-promise-executor': 'off',
  'space-before-function-paren': 'off',

  // Disable indentation rules that conflict with Prettier
  indent: 'off',
  '@typescript-eslint/indent': 'off',

  // Prettier integration
  'prettier/prettier': ['error', prettierConfig]
}

// Base ignores for all projects
export const sharedIgnores = [
  '**/build/*.ts',
  '**/.next/**',
  '**/node_modules/**',
  '**/dist/**',
  '**/coverage/**',
  '**/apps/web/public/static/**'
]

export function createExactPackageJsonDependencyVersionsConfig(lucyPlugin) {
  return {
    files: ['package.json'],
    languageOptions: {
      parser: jsoncEslintParser,
      parserOptions: {
        jsonSyntax: 'JSON'
      }
    },
    plugins: {
      lucy: lucyPlugin
    },
    rules: {
      'lucy/exact-package-json-dependency-versions': 'error'
    }
  }
}

const rootConfigMisuseError = new Error(
  'Run lint per-package: use `bun run lint`, or cd into the package. ' +
    'This root eslint.config.mjs is a shared-fragments library, not a config.'
)

// This file exports lint fragments consumed by the per-package
// eslint.config.mjs files; it is NOT a usable ESLint config. Without a default
// export ESLint silently runs an empty config and exits 0, so every root-cwd
// `eslint <path>` invocation passes without linting anything. A top-level
// throw would fire on import and break every per-package lint, so the throw is
// deferred to property access: importing the named fragments never touches the
// default export, while ESLint loading this file as a config always does.
export default new Proxy(
  {},
  {
    get() {
      throw rootConfigMisuseError
    },
    ownKeys() {
      throw rootConfigMisuseError
    }
  }
)

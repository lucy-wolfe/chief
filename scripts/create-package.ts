#!/usr/bin/env bun
/**
 * Scaffold a new standard workspace library package with the repo's canonical
 * TypeScript setup: tsconfig (composite build + vitest), the shared ESLint flat
 * config wiring, vitest, a dist-emitting build, and an exports map.
 *
 * Usage:
 *   bun scripts/create-package.ts --dir packages/foundation
 *   bun scripts/create-package.ts --dir packages/foo --name @chief/foo
 *
 * The generated package mirrors terminal's packages/core toolchain (same
 * ESLint stack, same Prettier — shared from the repo root — same build via
 * tsc + tsc-alias). Tooling devDependency versions are read from the ROOT
 * package.json (chief has no canonical `packages/core` to copy from — E0-S1
 * already exact-pins the shared tooling there), plus a small in-file table
 * for the handful of per-package-only tools the root doesn't pin.
 */
import { existsSync } from 'node:fs'
import { mkdir, readdir, readFile, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

// Tooling devDependencies every standard library package needs. Versions are
// read from the root package.json (the canonical pin source) so they stay in
// lockstep with the rest of the workspace.
const TOOLING_DEV_DEPENDENCIES = [
  '@eslint/eslintrc',
  '@eslint/js',
  '@typescript-eslint/eslint-plugin',
  '@typescript-eslint/parser',
  'concurrently',
  'eslint',
  'eslint-config-prettier',
  'eslint-plugin-import',
  'eslint-plugin-jasmine',
  'eslint-plugin-prettier',
  'prettier',
  'rimraf',
  'tsc-alias',
  'typescript',
  'vitest'
]

// The three tools every scaffolded package needs that the root package.json
// does not pin (terminal reads these from packages/core; chief has no
// canonical package to copy from yet). Versions from terminal
// packages/foundation; move to root pins when a canonical package exists.
const EXTRA_TOOLING_PINS: Record<string, string> = {
  concurrently: '9.2.1',
  rimraf: '6.1.3',
  'tsc-alias': '1.8.16'
}

type CliArgs = {
  dir: string
  name: string
}

function parseArgs(argv: string[]): CliArgs {
  let dir: string | undefined
  let name: string | undefined
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === '--dir') {
      dir = argv[++i]
    } else if (arg.startsWith('--dir=')) {
      dir = arg.slice('--dir='.length)
    } else if (arg === '--name') {
      name = argv[++i]
    } else if (arg.startsWith('--name=')) {
      name = arg.slice('--name='.length)
    }
  }
  if (dir === undefined || dir.length === 0) {
    throw new Error('Missing required --dir <path> (e.g. --dir packages/foundation)')
  }
  const normalizedDir = dir.replace(/\/+$/, '')
  return {
    dir: normalizedDir,
    name: name ?? `@chief/${basename(normalizedDir)}`
  }
}

// Number of path segments deep the package sits, so generated configs can point
// back at the repo root (e.g. packages/foundation → '../../').
function rootRelativePrefix(dir: string): string {
  const depth = dir.split('/').filter((segment) => segment.length > 0).length
  return '../'.repeat(depth)
}

async function readToolingVersions(): Promise<Record<string, string>> {
  const rootPackageJsonPath = join(REPO_ROOT, 'package.json')
  const raw = await readFile(rootPackageJsonPath, 'utf8')
  const parsed: { devDependencies?: Record<string, string> } = JSON.parse(raw)
  const rootDevDeps = parsed.devDependencies ?? {}
  const versions: Record<string, string> = {}
  for (const dependency of TOOLING_DEV_DEPENDENCIES) {
    const version = rootDevDeps[dependency] ?? EXTRA_TOOLING_PINS[dependency]
    if (version === undefined) {
      throw new Error(
        `neither the root package.json nor EXTRA_TOOLING_PINS pins devDependency "${dependency}"; cannot derive standard version`
      )
    }
    versions[dependency] = version
  }
  return versions
}

function packageJsonContent(name: string, toolingVersions: Record<string, string>): string {
  const devDependencies: Record<string, string> = {}
  for (const dependency of [...TOOLING_DEV_DEPENDENCIES].sort()) {
    devDependencies[dependency] = toolingVersions[dependency]
  }
  const pkg = {
    name,
    license: 'Apache-2.0',
    version: '0.0.0',
    private: true,
    type: 'module',
    files: ['dist'],
    exports: {
      '.': {
        types: './dist/index.d.ts',
        import: './dist/index.js'
      }
    },
    scripts: {
      build:
        'rimraf dist && rimraf tsconfig.build.tsbuildinfo && tsc -p tsconfig.build.json && tsc-alias -p tsconfig.build.json --resolve-full-paths',
      dev: 'concurrently "tsc -p tsconfig.build.json --watch --preserveWatchOutput" "tsc-alias -p tsconfig.build.json --watch --resolve-full-paths"',
      'test:unit': 'vitest run',
      'test:unit:coverage': 'vitest run --coverage',
      lint: 'eslint . package.json --ext .ts --max-warnings 0 --no-warn-ignored',
      'lint:fix': 'eslint . package.json --ext .ts --fix --max-warnings 0 --no-warn-ignored',
      format: 'prettier --write "**/*.{ts,tsx,md}"',
      'format:check': 'prettier --check "**/*.{ts,tsx,md}"'
    },
    dependencies: {},
    devDependencies
  }
  return `${JSON.stringify(pkg, null, 2)}\n`
}

function tsconfigContent(): string {
  const tsconfig = {
    compilerOptions: {
      composite: true,
      paths: {
        '@/*': ['./src/*']
      }
    },
    files: [],
    references: [{ path: './tsconfig.build.json' }, { path: './tsconfig.vitest.json' }]
  }
  return `${JSON.stringify(tsconfig, null, 2)}\n`
}

function tsconfigBuildContent(rootPrefix: string): string {
  const tsconfig = {
    extends: `${rootPrefix}tsconfig.base.json`,
    compilerOptions: {
      composite: true,
      incremental: true,
      outDir: 'dist',
      rootDir: 'src',
      declarationMap: true,
      paths: {
        '@/*': ['./src/*']
      }
    },
    include: ['src/**/*.ts'],
    exclude: ['node_modules', 'dist', 'src/**/*.test.ts']
  }
  return `${JSON.stringify(tsconfig, null, 2)}\n`
}

function tsconfigVitestContent(rootPrefix: string): string {
  const tsconfig = {
    extends: `${rootPrefix}tsconfig.base.json`,
    compilerOptions: {
      noEmit: true,
      paths: {
        '@/*': ['./src/*'],
        '@test/*': ['./test/*']
      }
    },
    include: ['vitest.config.ts', 'src/**/*.ts', 'test/**/*.ts']
  }
  return `${JSON.stringify(tsconfig, null, 2)}\n`
}

function vitestConfigContent(): string {
  return `import { defineConfig } from 'vitest/config'

export default defineConfig({
  resolve: {
    tsconfigPaths: true
  },
  test: {
    // #2982: refuse to run until this package's workspace dependencies are built,
    // so an unbuilt dep fails in one readable line instead of 100+ misleading
    // "Failed to resolve import" errors that read as a broken checkout.
    globalSetup: ['../../scripts/test/assert-workspace-built.mjs'],
    environment: 'node',
    include: ['test/**/*.test.ts'],
    passWithNoTests: true,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'json'],
      reportsDirectory: './coverage',
      include: ['src/**']
    }
  }
})
`
}

// Mirrors terminal's library baseline: shared TS rules + the generally-
// applicable lucy rules (the layer-specific ones — fetch scoping, direct-db
// access, etc. — are intentionally omitted; add them per package when the
// structure warrants). import/no-extraneous-dependencies is re-specified
// with a depth-correct packageDir so the package resolves both its own and
// the root devDeps.
function eslintConfigContent(rootPrefix: string): string {
  return `import { FlatCompat } from '@eslint/eslintrc'
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
import lucyPlugin from '${rootPrefix}packages/eslinter/index.js'
import {
  createExactPackageJsonDependencyVersionsConfig,
  sharedIgnores,
  sharedTypeScriptRules
} from '${rootPrefix}eslint.config.mjs'

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
          packageDir: ['.', '${rootPrefix}'],
          devDependencies: true
        }
      ],
      'lucy/no-json-stringify': 'error',
      'lucy/no-raw-null-check': 'error',
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
  {
    ignores: sharedIgnores
  }
]

export default eslintConfig
`
}

// A real (non re-export) placeholder so the entry lints clean immediately —
// lucy/no-empty-file and lucy/no-barrel-re-export both reject the obvious
// alternatives (empty file / pure re-export barrel). Replace with real exports.
function indexContent(name: string): string {
  return `export const PACKAGE_NAME = '${name}'
`
}

function readmeContent(name: string, dir: string): string {
  return `# ${name}

Scaffolded with \`bun scripts/create-package.ts --dir ${dir}\`.

Replace \`src/index.ts\` with the package's real exports.
`
}

// Register the package in the root tsconfig.json references so editor tooling
// and `tsc -b` at the root pick it up. Idempotent.
async function registerInRootTsconfig(dir: string): Promise<boolean> {
  const rootTsconfigPath = join(REPO_ROOT, 'tsconfig.json')
  const raw = await readFile(rootTsconfigPath, 'utf8')
  const parsed: { files?: string[]; references?: { path: string }[] } = JSON.parse(raw)
  const references = parsed.references ?? []
  if (references.some((reference) => reference.path === dir)) {
    return false
  }
  references.push({ path: dir })
  parsed.references = references
  await writeFile(rootTsconfigPath, `${JSON.stringify(parsed, null, 2)}\n`, 'utf8')
  return true
}

async function isEmptyDir(path: string): Promise<boolean> {
  const entries = await readdir(path)
  return entries.length === 0
}

async function createPackage(args: CliArgs): Promise<void> {
  const packageDir = join(REPO_ROOT, args.dir)
  if (existsSync(packageDir) && !(await isEmptyDir(packageDir))) {
    throw new Error(`Target directory "${args.dir}" already exists and is not empty`)
  }

  const rootPrefix = rootRelativePrefix(args.dir)
  const toolingVersions = await readToolingVersions()

  await mkdir(join(packageDir, 'src'), { recursive: true })

  const files: Record<string, string> = {
    'package.json': packageJsonContent(args.name, toolingVersions),
    'tsconfig.json': tsconfigContent(),
    'tsconfig.build.json': tsconfigBuildContent(rootPrefix),
    'tsconfig.vitest.json': tsconfigVitestContent(rootPrefix),
    'vitest.config.ts': vitestConfigContent(),
    'eslint.config.mjs': eslintConfigContent(rootPrefix),
    'README.md': readmeContent(args.name, args.dir),
    'src/index.ts': indexContent(args.name)
  }

  for (const [relativePath, content] of Object.entries(files)) {
    await writeFile(join(packageDir, relativePath), content, 'utf8')
  }

  const registered = await registerInRootTsconfig(args.dir)

  process.stdout.write(`Scaffolded ${args.name} at ${args.dir}\n`)
  for (const relativePath of Object.keys(files)) {
    process.stdout.write(`  + ${args.dir}/${relativePath}\n`)
  }
  process.stdout.write(
    registered
      ? `  ~ tsconfig.json (added reference to ${args.dir})\n`
      : `  = tsconfig.json (reference already present)\n`
  )
  process.stdout.write('\nNext steps:\n')
  process.stdout.write('  1. bun install   (link the new workspace)\n')
  process.stdout.write(`  2. Replace ${args.dir}/src/index.ts with real exports\n`)
  process.stdout.write(`  3. bun run lint --filter=${args.name}\n`)
}

if (import.meta.main) {
  void createPackage(parseArgs(process.argv.slice(2))).catch((error: unknown) => {
    process.stderr.write(
      `[create-package] ${String(error instanceof Error ? error.message : error)}\n`
    )
    process.exit(1)
  })
}

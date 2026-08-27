import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { defineConfig } from 'vitest/config'

const dirname = path.dirname(fileURLToPath(import.meta.url))

export default defineConfig({
  resolve: {
    alias: {
      '@': path.resolve(dirname, './src'),
      '@test': path.resolve(dirname, './test')
    }
  },
  // `@chief/piing` exports its Pi extensions as SOURCE (chiefd materializes
  // those exact files into every Pi home), so Vitest must transform them
  // instead of handing them to Node as if they were built. Next does the same
  // through `transpilePackages` — one registration, two loaders.
  ssr: {
    noExternal: ['@chief/piing']
  },
  // The app tsconfig sets `jsx: preserve` (required by Next.js), which makes Vitest's
  // oxc transform leave JSX untransformed and fail to parse .tsx test/source files.
  // Override the JSX runtime here so component tests can render React.
  oxc: {
    jsx: {
      runtime: 'automatic',
      importSource: 'react'
    }
  },
  test: {
    // Node 26 puts `localStorage`/`sessionStorage` on the global by default,
    // and both read as `undefined` without `--localstorage-file`. Vitest's
    // jsdom environment copies a window key onto the global only when the
    // global does not hold that key already (`populateGlobal`'s
    // `if (k in global) return keysArray.includes(k)`, and neither storage
    // key is in its built-in list), so Node's dead accessor wins and jsdom's
    // real `Storage` never lands: every `localStorage` read in a jsdom test
    // is `undefined`. Turn Node's Web Storage off in the workers instead, so
    // jsdom owns the name. Removing this makes the storage assertions in
    // test/services/SessionClientService.test.ts throw on Node 26 while they
    // still pass on an older Node — the exact local-only red this replaces.
    // (Top-level, not `poolOptions`: Vitest 4 removed that nesting and warns
    // "`test.poolOptions` was removed" while silently ignoring what is inside.)
    execArgv: ['--no-experimental-webstorage'],
    // #2982: refuse to run until @chief/chiefing (this app's one workspace
    // dependency) is built, so an unbuilt dep fails in one readable line
    // instead of 100+ misleading "Failed to resolve import" errors.
    globalSetup: ['../../scripts/test/assert-workspace-built.mjs'],
    setupFiles: ['./test/Setup.ts'],
    include: ['test/**/*.test.ts', 'test/**/*.test.tsx'],
    passWithNoTests: true,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json', 'json-summary'],
      reportsDirectory: './coverage'
    }
  }
})

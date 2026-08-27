import { fileURLToPath } from 'node:url'

import { defineConfig } from 'vitest/config'

export default defineConfig({
  resolve: {
    tsconfigPaths: true,
    alias: {
      '@test-assets': fileURLToPath(new URL('./extensions', import.meta.url))
    }
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

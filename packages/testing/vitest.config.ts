import { defineConfig } from 'vitest/config'

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
    // This package's own self-tests boot the REAL chiefd binary as a
    // subprocess — the default 5s per-test timeout is too tight for a real
    // process spawn + port bind + health poll.
    testTimeout: 20_000,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'json'],
      reportsDirectory: './coverage',
      include: ['src/**']
    }
  }
})

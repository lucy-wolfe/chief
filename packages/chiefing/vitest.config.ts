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
    // A6(c). `test/contract/**` boots the REAL chiefd binary as a subprocess,
    // and since the org route family left the unauthenticated docstore-only
    // mount it boots the COMPANY surface (`chiefd run --serve-only`): a company
    // database open, two minted keypairs and the whole route table, whose
    // reachability budget alone is 20s. Vitest's 5s default was ample for the
    // docstore-only daemon these suites used to spawn and is not ample for this
    // one. Same reasoning, and the same shape, as `packages/testing`'s own
    // config -- that package boots the identical harness for its self-tests.
    //
    // A ceiling, not a budget: nothing here is expected to take 45s, and this
    // exists so a wedged daemon reports as a failed test instead of as a
    // timeout in a hook nobody can attribute.
    testTimeout: 45_000,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'json'],
      reportsDirectory: './coverage',
      include: ['src/**']
    }
  }
})

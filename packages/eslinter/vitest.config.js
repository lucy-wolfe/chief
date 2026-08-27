import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    globalSetup: ['../../scripts/test/assert-workspace-built.mjs'],
    environment: 'node',
    include: ['test/**/*.test.mjs'],
    passWithNoTests: true,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'json'],
      reportsDirectory: './coverage',
      include: ['rules/**', 'index.js', 'utils.js']
    }
  }
})

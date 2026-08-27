// The repo `.env` must reach node-launched dev wrappers.
//
// Regression: `bun run web:dev` runs `scripts/dev-web.mjs` under node, which
// (unlike bun) does not read `.env`. Every provider key an operator had put
// there was therefore invisible to every child a node wrapper spawned, on a
// box whose `.env` was correct.

import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { loadRepoEnv } from '../repo-env-lib.mjs'

/** A throwaway repo root holding the given `.env` text. */
function repoWithEnv(contents) {
  const root = mkdtempSync(join(tmpdir(), 'chief-repo-env-'))
  writeFileSync(join(root, '.env'), contents)
  return root
}

test('fills the provider keys from the repo .env', () => {
  const root = repoWithEnv('OPENROUTER_API_KEY=sk-or-from-file\nFAL_KEY=fal-from-file\n')
  /** @type {Record<string, string>} */
  const environment = {}
  const loaded = loadRepoEnv(root, environment)
  assert.equal(environment.OPENROUTER_API_KEY, 'sk-or-from-file')
  assert.equal(environment.FAL_KEY, 'fal-from-file')
  assert.deepEqual([...loaded].sort(), ['FAL_KEY', 'OPENROUTER_API_KEY'])
})

test('a shell export wins over the file', () => {
  const root = repoWithEnv('OPENROUTER_API_KEY=sk-or-from-file\n')
  const environment = { OPENROUTER_API_KEY: 'sk-or-exported' }
  assert.deepEqual(loadRepoEnv(root, environment), [])
  assert.equal(environment.OPENROUTER_API_KEY, 'sk-or-exported')
})

test('no .env is not an error', () => {
  const root = mkdtempSync(join(tmpdir(), 'chief-repo-env-'))
  /** @type {Record<string, string>} */
  const environment = {}
  assert.deepEqual(loadRepoEnv(root, environment), [])
  assert.deepEqual(environment, {})
})

test('dev-web loads the repo env before it spawns anything', () => {
  const source = readFileSync(
    fileURLToPath(new URL('../dev-web.mjs', import.meta.url)),
    'utf8',
  )
  const load = source.indexOf('loadRepoEnv(REPO_ROOT)')
  const firstRead = source.indexOf('process.env.')
  assert.ok(load > 0, 'dev-web.mjs must call loadRepoEnv(REPO_ROOT)')
  assert.ok(
    load < firstRead,
    'loadRepoEnv must run before dev-web.mjs reads any env var',
  )
})

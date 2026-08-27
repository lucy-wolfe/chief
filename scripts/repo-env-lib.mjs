// The repo `.env`, loaded for node-launched dev wrappers.
//
// `.env.example` is where an operator is told to put their provider keys, and
// `bun` reads `.env` on its own — but `scripts/*.mjs` run under node, which
// does not. Every child a node wrapper spawns therefore saw none of it, and a
// registry naming `"apiKey": "$OPENROUTER_API_KEY"` resolved to nothing on a
// box whose `.env` set that key perfectly well.
//
// The file is a DEFAULT, not an override: a real shell export always wins, so
// `OPENROUTER_API_KEY=x bun run web:dev` still means x.
//
// A Founder ROUTE is deliberately not among the things this can carry: a
// company runs on whatever the operator's own Pi is on, so there is nothing
// here for an operator to set and get wrong.

import { existsSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parseEnv } from 'node:util'

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

/**
 * Fill unset variables from `<root>/.env`. No file is not an error — a box
 * that exports its route in the shell needs no file at all.
 *
 * @param {string} [root] repository root holding the `.env`
 * @param {Record<string, string | undefined>} [env] the environment to fill
 * @returns {string[]} the names actually set, in no guaranteed order
 */
export function loadRepoEnv(root = REPO_ROOT, env = process.env) {
  const path = join(root, '.env')
  if (!existsSync(path)) return []
  const loaded = []
  for (const [key, value] of Object.entries(parseEnv(readFileSync(path, 'utf8')))) {
    if (env[key] !== undefined) continue
    env[key] = value
    loaded.push(key)
  }
  return loaded
}

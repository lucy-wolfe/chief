// The TS equivalent of chiefd's clippy.toml disallowed-methods discipline
// (blocking-inventory §G3): proves the retired sync stack is ABSENT, not
// merely unused. Each symbol gets its own it() so a failure names exactly
// what came back. Stays green after every later story lands in this package.

import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

const here = dirname(fileURLToPath(import.meta.url))
const srcDir = join(here, '..', '..', 'src')
const discoveryPrefix = join(srcDir, 'discovery') + sep

function listFilesRecursive(dir: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) {
      out.push(...listFilesRecursive(full))
    } else if (entry.name.endsWith('.ts')) {
      out.push(full)
    }
  }
  return out
}

const allFiles = listFilesRecursive(srcDir)
// Ruling D1/D7's URL-fallback assertions are scoped outside src/discovery —
// E10-S4 owns that directory and carries its own stricter greps (it legitimately
// compiles in DEFAULT_BEACOND_URL = 'http://127.0.0.1:6969').
const nonDiscoveryFiles = allFiles.filter((file) => !file.startsWith(discoveryPrefix))

function assertAbsent(pattern: RegExp, files: readonly string[] = allFiles): void {
  const hits = files.filter((file) => pattern.test(readFileSync(file, 'utf8')))
  expect(hits).toEqual([])
}

describe('no blocking primitives anywhere under src/', () => {
  it('the blocking Atomics method is absent', () =>
    assertAbsent(new RegExp(['Atomics', 'wait'].join('\\.'))))
  it('spawnSync is absent', () => assertAbsent(/spawnSync/))
  it('setInterval is absent', () => assertAbsent(/setInterval/))
  it('process.env is absent', () => assertAbsent(/process\.env/))
  it('node:child_process is absent', () => assertAbsent(/node:child_process/))
  it('bun:ffi is absent', () => assertAbsent(/bun:ffi/))
  it('SharedArrayBuffer is absent', () => assertAbsent(/SharedArrayBuffer/))
})

describe('the retired sync transport stack never reappears', () => {
  it('SyncHttpTransport is absent', () => assertAbsent(/SyncHttpTransport/))
  it('CurlHttpTransport is absent', () => assertAbsent(/CurlHttpTransport/))
  it('CurlInvoker is absent', () => assertAbsent(/CurlInvoker/))
  it('inProcessFetchInvoker is absent', () => assertAbsent(/inProcessFetchInvoker/))
  it('synchronousSleep is absent', () => assertAbsent(/synchronousSleep/))
  it('durable-store-fetch-worker is absent', () => assertAbsent(/durable-store-fetch-worker/))
})

describe('WriteServiceUnavailableError never reappears', () => {
  it('is absent', () => assertAbsent(/WriteServiceUnavailableError/))
})

describe('rulings D1/D7 — no fixed-port fallback outside src/discovery', () => {
  it('DEFAULT_CHIEFD_URL is absent outside discovery', () =>
    assertAbsent(/DEFAULT_CHIEFD_URL/, nonDiscoveryFiles))
  it('ORG_CHIEFD_URL is absent outside discovery', () =>
    assertAbsent(/ORG_CHIEFD_URL/, nonDiscoveryFiles))
  it('127.0.0.1:8792 is absent outside discovery', () =>
    assertAbsent(/127\.0\.0\.1:8792/, nonDiscoveryFiles))
})

describe('classification is structural, never a message regex', () => {
  it('error text is never matched, tested, or searched', () =>
    assertAbsent(new RegExp(['\\.mes', 'sage\\b.*', '(match|test\\(|incl', 'udes\\()'].join(''))))
})

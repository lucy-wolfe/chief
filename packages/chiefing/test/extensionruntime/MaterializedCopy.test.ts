import { spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

import { materializeExtensionRuntimeCopy } from '@test/extensionruntime/Closure'
import { afterEach, describe, expect, it } from 'vitest'

const require = createRequire(import.meta.url)
const tscPath = require.resolve('typescript/bin/tsc')

describe('extension-runtime materialized copy', () => {
  const scratchDirs: string[] = []

  afterEach(() => {
    for (const dir of scratchDirs.splice(0)) rmSync(dir, { recursive: true, force: true })
  })

  it('typechecks the real flattened graph without node_modules or tsconfig paths', () => {
    const destination = mkdtempSync(join(tmpdir(), 'chiefing-extension-runtime-'))
    scratchDirs.push(destination)
    const copy = materializeExtensionRuntimeCopy(destination)

    expect(copy.closure.length).toBeGreaterThan(1)
    expect(existsSync(copy.copiedEntry)).toBe(true)
    expect(existsSync(resolve(destination, 'node_modules'))).toBe(false)
    expect(readFileSync(resolve(destination, 'tsconfig.json'), 'utf8')).not.toContain('paths')
    expect(readFileSync(copy.copiedEntry, 'utf8')).toContain('./chiefing-runtime-Rendezvous.ts')
    expect(readFileSync(copy.copiedEntry, 'utf8')).toContain('postOrgRoute')

    for (const entry of copy.closure) {
      const copied = resolve(destination, `chiefing-runtime-${entry.split('/').at(-1)}`)
      expect(readFileSync(copied, 'utf8')).not.toContain('@chief/')
    }

    const result = spawnSync(
      process.execPath,
      [tscPath, '--noEmit', '--project', 'tsconfig.json'],
      {
        cwd: destination,
        encoding: 'utf8',
        timeout: 20_000
      }
    )
    expect(result.error).toBeUndefined()
    expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(0)
    expect(result.stdout).toBe('')
    expect(result.stderr).toBe('')
  }, 30_000)
})

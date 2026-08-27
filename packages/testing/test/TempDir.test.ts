import { existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, dirname } from 'node:path'

import { describe, expect, it } from 'vitest'

import { createTempDir } from '@/TempDir'

describe('createTempDir', () => {
  it('creates a directory under the OS temp dir with the given prefix', async () => {
    const root = await createTempDir('my-prefix-')
    expect(existsSync(root.path)).toBe(true)
    expect(dirname(root.path)).toBe(tmpdir())
    expect(basename(root.path).startsWith('my-prefix-')).toBe(true)
    await root.remove()
  })

  it('defaults to the chief-daemon-test- prefix', async () => {
    const root = await createTempDir()
    expect(basename(root.path).startsWith('chief-daemon-test-')).toBe(true)
    await root.remove()
  })

  it('remove() deletes the directory recursively', async () => {
    const root = await createTempDir()
    const { writeFile, mkdir } = await import('node:fs/promises')
    const { join } = await import('node:path')
    await mkdir(join(root.path, 'nested'), { recursive: true })
    await writeFile(join(root.path, 'nested', 'file.txt'), 'hello')

    await root.remove()

    expect(existsSync(root.path)).toBe(false)
  })

  /**
   * THE TEARDOWN RACE, from the failing side.
   *
   * Observed live: `OrganizationToolContract` passed all 16 of its tests and
   * then reported the whole FILE as failed —
   * `ENOTEMPTY: directory not empty, rmdir '.../orgs/toolcontract'` — because
   * `tmux kill-server` returns while the server is still reaping pane process
   * groups, and each of those panes is a Pi home still being written under the
   * tree this is removing. A suite that reports a failure nothing in the
   * product caused is worse than no teardown at all: it trains the reader to
   * discount the result.
   *
   * The writer here stands in for those dying panes. It keeps creating files
   * inside the tree while the removal walks it, and stops as soon as the
   * directory is gone. Without the retry budget the removal loses that race
   * and throws; with it, the removal wins and the tree is gone.
   */
  it('remove() survives a tree that is still being written while it walks', async () => {
    const root = await createTempDir()
    const { mkdir, writeFile } = await import('node:fs/promises')
    const { join } = await import('node:path')
    const nested = join(root.path, 'orgs', 'company', 'people', 'ceo')
    await mkdir(nested, { recursive: true })
    for (let index = 0; index < 200; index += 1) {
      await writeFile(join(nested, `seed-${index}.txt`), 'x')
    }

    let writing = true
    const writer = (async () => {
      for (let index = 0; writing && index < 5_000; index += 1) {
        try {
          await writeFile(join(nested, `late-${index}.txt`), 'x')
        } catch {
          // ENOENT once the directory is gone: the race is over, not failed.
          return
        }
      }
    })()

    await expect(root.remove()).resolves.toBeUndefined()
    writing = false
    await writer

    expect(existsSync(root.path)).toBe(false)
  })

  it('remove() is idempotent', async () => {
    const root = await createTempDir()
    await root.remove()
    await expect(root.remove()).resolves.toBeUndefined()
  })
})

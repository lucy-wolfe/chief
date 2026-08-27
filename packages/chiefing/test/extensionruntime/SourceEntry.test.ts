import { spawnSync } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync
} from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

import ts from 'typescript'
import { afterEach, describe, expect, it } from 'vitest'

import { chiefingExtensionRuntimeSourceEntry } from '@/index'
import { isNullish } from '@/Nullish'

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const repositoryRoot = resolve(packageRoot, '..', '..')
const sourceEntry = resolve(packageRoot, 'src', 'extensionruntime', 'index.ts')
const require = createRequire(import.meta.url)
const tscPath = require.resolve('typescript/bin/tsc')

/* eslint-disable lucy/no-process-env */
// This test-only harness must direct `os.tmpdir()` to a checkout and inspect
// the platform's fallback roots, so its environment access is centralized in
// these helpers rather than leaking direct reads and writes through the suite.
function readEnvironmentVariable(name: string): string | undefined {
  return process.env[name]
}

function writeEnvironmentVariable(name: string, value: string | undefined): void {
  if (typeof value === 'string') process.env[name] = value
  else delete process.env[name]
}
/* eslint-enable lucy/no-process-env */

function isInside(root: string, candidate: string): boolean {
  const pathFromRoot = relative(realpathSync(root), realpathSync(candidate))
  return pathFromRoot === '' || (!pathFromRoot.startsWith('..') && !isAbsolute(pathFromRoot))
}

function firstExternalAncestor(path: string): string | undefined {
  if (!existsSync(path)) return undefined

  let candidate = realpathSync(path)
  while (isInside(repositoryRoot, candidate)) {
    const parent = dirname(candidate)
    if (parent === candidate) return undefined
    candidate = realpathSync(parent)
  }
  return candidate
}

function platformTempFallbacks(): readonly string[] {
  if (process.platform !== 'win32') return ['/tmp', '/var/tmp']

  const localAppData = readEnvironmentVariable('LOCALAPPDATA')
  const systemRoot = readEnvironmentVariable('SystemRoot')
  const windowsDirectory = readEnvironmentVariable('windir')
  return [
    localAppData && join(localAppData, 'Temp'),
    systemRoot && join(systemRoot, 'Temp'),
    windowsDirectory && join(windowsDirectory, 'Temp')
  ].filter((candidate): candidate is string => !isNullish(candidate))
}

function externalScratchParentCandidates(): readonly string[] {
  const configuredTemp = tmpdir()
  const candidates = [
    configuredTemp,
    firstExternalAncestor(configuredTemp),
    ...platformTempFallbacks()
  ]
  return [...new Set(candidates)]
    .filter((candidate): candidate is string => !isNullish(candidate) && existsSync(candidate))
    .filter((candidate) => !isInside(repositoryRoot, candidate))
}

function createConsumerScratchDir(): string {
  let lastError: unknown
  for (const parent of externalScratchParentCandidates()) {
    let consumer: string | undefined
    try {
      consumer = mkdtempSync(join(parent, 'chiefing-extension-runtime-consumer-'))
      const packageLink = resolve(consumer, 'node_modules', '@chief', 'chiefing')
      mkdirSync(dirname(packageLink), { recursive: true })
      symlinkSync(packageRoot, packageLink, process.platform === 'win32' ? 'junction' : 'dir')
      return consumer
    } catch (error) {
      if (consumer) rmSync(consumer, { recursive: true, force: true })
      lastError = error
    }
  }
  throw new Error(
    `could not create consumer scratch outside ${repositoryRoot}: ${String(lastError)}`
  )
}

function repositoryStatus(): string {
  const result = spawnSync('git', ['status', '--porcelain'], {
    cwd: repositoryRoot,
    encoding: 'utf8'
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`git status failed: ${result.stdout}\n${result.stderr}`)
  }
  return result.stdout
}

function hasSourceEntry(
  value: unknown
): value is { chiefingExtensionRuntimeSourceEntry: () => string } {
  return (
    typeof value === 'object' &&
    !isNullish(value) &&
    'chiefingExtensionRuntimeSourceEntry' in value &&
    typeof value.chiefingExtensionRuntimeSourceEntry === 'function'
  )
}

describe('chiefingExtensionRuntimeSourceEntry', () => {
  const scratchDirs: string[] = []

  afterEach(() => {
    for (const dir of scratchDirs.splice(0)) rmSync(dir, { recursive: true, force: true })
  })

  it('keeps consumer scratch outside the real checkout and leaves no status residue when TMPDIR points inside it', () => {
    const originalTmpdir = readEnvironmentVariable('TMPDIR')
    const originalTmp = readEnvironmentVariable('TMP')
    const originalTemp = readEnvironmentVariable('TEMP')
    const statusBefore = repositoryStatus()
    try {
      writeEnvironmentVariable('TMPDIR', repositoryRoot)
      writeEnvironmentVariable('TMP', repositoryRoot)
      writeEnvironmentVariable('TEMP', repositoryRoot)
      const consumer = createConsumerScratchDir()
      scratchDirs.push(consumer)
      writeFileSync(resolve(consumer, 'cancellation-sentinel.ts'), 'export {}\n', 'utf8')

      expect({
        consumerInsideRepository: isInside(repositoryRoot, consumer),
        status: repositoryStatus()
      }).toEqual({
        consumerInsideRepository: false,
        status: statusBefore
      })
    } finally {
      writeEnvironmentVariable('TMPDIR', originalTmpdir)
      writeEnvironmentVariable('TMP', originalTmp)
      writeEnvironmentVariable('TEMP', originalTemp)
    }
  })

  it('returns the real source entry from a source load and an emitted dist load', async () => {
    expect(chiefingExtensionRuntimeSourceEntry()).toBe(sourceEntry)

    const distIndex = resolve(packageRoot, 'dist', 'index.js')
    expect(existsSync(distIndex)).toBe(true)
    const dist = await import(pathToFileURL(distIndex).href)
    if (!hasSourceEntry(dist)) throw new Error('emitted chiefing barrel lacks source entry export')
    expect(dist.chiefingExtensionRuntimeSourceEntry()).toBe(sourceEntry)
  })

  it('makes the package subpath resolvable to a scratch consumer after build', () => {
    const consumer = createConsumerScratchDir()
    scratchDirs.push(consumer)
    writeFileSync(
      resolve(consumer, 'index.ts'),
      "import { readDaemonRendezvous, subscribeSse } from '@chief/chiefing/extension-runtime'\n" +
        "const company = readDaemonRendezvous('/work/acme')\n" +
        'void company\n' +
        'void subscribeSse\n',
      'utf8'
    )
    writeFileSync(
      resolve(consumer, 'tsconfig.json'),
      '{\n' +
        '  "compilerOptions": {\n' +
        '    "target": "ESNext",\n' +
        '    "module": "NodeNext",\n' +
        '    "moduleResolution": "NodeNext",\n' +
        '    "noEmit": true,\n' +
        '    "skipLibCheck": true,\n' +
        '    "strict": true\n' +
        '  },\n' +
        '  "files": ["index.ts"]\n' +
        '}\n',
      'utf8'
    )
    writeFileSync(resolve(consumer, 'package.json'), '{"type":"module"}\n', 'utf8')

    const result = spawnSync(process.execPath, [tscPath, '--project', 'tsconfig.json'], {
      cwd: consumer,
      encoding: 'utf8',
      timeout: 20_000
    })
    expect(result.error).toBeUndefined()
    expect(result.status, `${result.stdout}\n${result.stderr}`).toBe(0)
    expect(result.stdout).toBe('')
    expect(result.stderr).toBe('')
  }, 30_000)

  it('builds chiefing itself before the root unit lane asserts its emitted export', () => {
    const turboPath = resolve(packageRoot, '..', '..', 'turbo.json')
    const parsed = ts.parseConfigFileTextToJson(turboPath, readFileSync(turboPath, 'utf8'))
    expect(parsed.error).toBeUndefined()
    expect(parsed.config).toMatchObject({
      tasks: {
        '@chief/chiefing#test:unit': {
          dependsOn: expect.arrayContaining(['build', '^build'])
        }
      }
    })
  })
})

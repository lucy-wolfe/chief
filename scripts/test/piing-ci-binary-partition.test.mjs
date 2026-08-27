// Piing has a large binary-independent test set and a small real-daemon set.
// The first set can start after the scope guard. The second set must wait for
// build-chiefd. Derive that boundary from imports so a new transitive binary
// dependency cannot silently enter an early shard.

import assert from 'node:assert/strict'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, extname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

import ts from 'typescript'

import { jobBlock, jobNeeds } from '../ci-sequence.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')

const BINARY_MODULES = [
  'packages/testing/src/ChiefdBinary.ts',
  'packages/testing/src/TmuxHostedCompanyDaemon.ts',
  'packages/testing/src/CompanyDaemon.ts',
]

const BINARY_TESTS = [
  'packages/piing/test/toolcontract/EnforcedGateToolSurfaceContract.test.ts',
  'packages/piing/test/toolcontract/OrganizationToolContract.test.ts',
  'packages/piing/test/toolcontract/ReminderDeliveryContract.test.ts',
  'packages/piing/test/toolcontract/RendezvousWriterBytesContract.test.ts',
]

function posixPath(path) {
  return path.split(sep).join('/')
}

function filesBelow(root, predicate) {
  if (!existsSync(root)) return []
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = join(root, entry.name)
    return entry.isDirectory() ? filesBelow(path, predicate) : predicate(path) ? [path] : []
  })
}

function packageRootFor(path, root) {
  for (const packageName of ['piing', 'testing', 'chiefing']) {
    const packageRoot = join(root, 'packages', packageName)
    if (path === packageRoot || path.startsWith(`${packageRoot}${sep}`)) return packageRoot
  }
  return undefined
}

function resolveFile(base) {
  const candidates = extname(base)
    ? [base]
    : [base, `${base}.ts`, `${base}.tsx`, `${base}.mts`, join(base, 'index.ts'), join(base, 'index.tsx')]
  return candidates.find((candidate) => existsSync(candidate))
}

function resolveImport(specifier, importer, root) {
  let base
  if (specifier.startsWith('.')) {
    base = resolve(dirname(importer), specifier)
  } else if (specifier.startsWith('@test/')) {
    base = join(root, 'packages', 'piing', 'test', specifier.slice('@test/'.length))
  } else if (specifier.startsWith('@test-assets/')) {
    base = join(root, 'packages', 'piing', specifier.slice('@test-assets/'.length))
  } else if (specifier.startsWith('@/')) {
    const packageRoot = packageRootFor(importer, root)
    if (!packageRoot) return undefined
    base = join(packageRoot, 'src', specifier.slice(2))
  } else if (specifier === '@chief/testing') {
    base = join(root, 'packages', 'testing', 'src', 'index')
  } else if (specifier === '@chief/chiefing') {
    base = join(root, 'packages', 'chiefing', 'src', 'index')
  } else if (specifier === '@chief/piing') {
    base = join(root, 'packages', 'piing', 'src', 'index')
  } else if (specifier === '@chief/piing/extension-runtime') {
    base = join(root, 'packages', 'piing', 'src', 'extensionruntime', 'index')
  } else if (specifier.startsWith('@chief/piing/extensions/')) {
    base = join(root, 'packages', 'piing', 'extensions', specifier.slice('@chief/piing/extensions/'.length))
  } else {
    return undefined
  }
  return resolveFile(base)
}

function importsOf(path, root) {
  const source = readFileSync(path, 'utf8')
  const info = ts.preProcessFile(source, true, true)
  return [...info.importedFiles, ...info.referencedFiles]
    .map(({ fileName }) => resolveImport(fileName, path, root))
    .filter((resolved) => resolved !== undefined)
}

function reachesBinary(start, root, targets) {
  const pending = [start]
  const seen = new Set()
  while (pending.length > 0) {
    const path = pending.pop()
    if (path === undefined || seen.has(path)) continue
    seen.add(path)
    if (targets.has(path)) return true
    pending.push(...importsOf(path, root))
  }
  return false
}

function derivePartition(root) {
  const testRoot = join(root, 'packages', 'piing', 'test')
  const tests = filesBelow(testRoot, (path) => path.endsWith('.test.ts')).sort()
  const targets = new Set(BINARY_MODULES.map((path) => join(root, ...path.split('/'))))
  const dependent = tests.filter((path) => reachesBinary(path, root, targets))
  const independent = tests.filter((path) => !dependent.includes(path))
  const relativeToRoot = (path) => posixPath(relative(root, path))
  return {
    all: tests.map(relativeToRoot),
    dependent: dependent.map(relativeToRoot),
    independent: independent.map(relativeToRoot),
  }
}

test('the Piing binary partition is complete, disjoint, non-vacuous, and pinned to the real import graph', () => {
  const partition = derivePartition(repoRoot)
  // 90 -> 80: provider/model management deleted ten Piing test files whose
  // subject went with the feature. A floor, not an inventory — it exists so a
  // glob that stops resolving fails by name, and it follows the tree down
  // rather than the tree following it.
  assert.ok(partition.all.length > 80, `expected the Piing suite, found ${partition.all.length} test files`)
  assert.deepEqual(partition.dependent, BINARY_TESTS)
  assert.ok(partition.independent.length > 80, 'the binary-independent lane is unexpectedly empty or small')
  assert.deepEqual(
    [...partition.independent, ...partition.dependent].sort(),
    partition.all,
    'the two lanes must cover every Piing test file',
  )
  assert.equal(
    new Set([...partition.independent, ...partition.dependent]).size,
    partition.all.length,
    'a Piing test file appears in both lanes',
  )
})

test('the derivation follows transitive imports instead of checking only a test file imports', () => {
  const fixture = mkdtempSync(join(tmpdir(), 'piing-binary-partition-'))
  try {
    const testFile = join(fixture, 'packages/piing/test/Transitive.test.ts')
    const supportFile = join(fixture, 'packages/piing/test/support/Middle.ts')
    const testingIndex = join(fixture, 'packages/testing/src/index.ts')
    const binaryFile = join(fixture, 'packages/testing/src/ChiefdBinary.ts')
    for (const path of [testFile, supportFile, testingIndex, binaryFile]) mkdirSync(dirname(path), { recursive: true })
    writeFileSync(testFile, "import '@test/support/Middle'\n")
    writeFileSync(supportFile, "import '@chief/testing'\n")
    writeFileSync(testingIndex, "export * from '@/ChiefdBinary'\n")
    writeFileSync(binaryFile, 'export const binary = true\n')

    assert.ok(
      !importsOf(testFile, fixture).includes(binaryFile),
      'the fixture must keep the binary dependency out of the direct import set',
    )
    assert.deepEqual(derivePartition(fixture).dependent, ['packages/piing/test/Transitive.test.ts'])
  } finally {
    rmSync(fixture, { recursive: true, force: true })
  }
})

test('ci.yml starts independent Piing shards after guard and assigns each binary test exactly once', () => {
  const workflow = readFileSync(join(repoRoot, '.github', 'workflows', 'ci.yml'), 'utf8')
  const shard = jobBlock(workflow, 'test-unit-piing')
  const contract = jobBlock(workflow, 'test-unit-piing-contract')
  assert.ok(shard)
  assert.ok(contract)

  assert.deepEqual(jobNeeds(workflow, 'test-unit-piing'), ['guard'])
  assert.doesNotMatch(shard, /build-chiefd|download-artifact|chiefd-ci-binary/)
  const excluded = [...shard.matchAll(/--exclude='([^']+)'/g)].map((match) => `packages/piing/${match[1]}`).sort()
  assert.deepEqual(excluded, BINARY_TESTS)

  assert.deepEqual(jobNeeds(workflow, 'test-unit-piing-contract'), ['guard', 'build-chiefd'])
  const ordered = /ordered\)([\s\S]*?);;/.exec(contract)?.[1] ?? ''
  const otherLanes = contract.replace(/ordered\)([\s\S]*?);;/, '')
  for (const path of BINARY_TESTS) {
    const packagePath = path.replace('packages/piing/', '')
    assert.match(ordered, new RegExp(packagePath.replaceAll('.', '\\.')))
  }
  for (const path of BINARY_TESTS.filter((path) => !path.endsWith('OrganizationToolContract.test.ts'))) {
    const packagePath = path.replace('packages/piing/', '')
    assert.doesNotMatch(otherLanes, new RegExp(packagePath.replaceAll('.', '\\.')))
  }
  assert.match(ordered, /durable reminder delivery, in an isolated live company/)
  assert.match(ordered, /the organization tool surface installs under an enforced auth gate/)
  assert.match(contract, /-- \$files --testNamePattern="\$pattern"/)
  assert.doesNotMatch(contract, /--exclude=/)

  assert.deepEqual(jobNeeds(workflow, 'test-unit'), [
    'guard',
    'test-unit-base',
    'test-unit-chiefd',
    'test-unit-piing',
    'test-unit-piing-contract',
  ])
})

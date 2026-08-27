// #873: every package a workspace member IMPORTS must be one it DECLARES.
//
// #800 imported `@chief/piing` while `apps/api/package.json` declared only
// `@chief/chiefing`. Bun's workspace install resolves a sibling from a WARM
// tree, so five gates were honestly green on the author's machine; a CLEAN
// install (CI, every fresh clone) cannot resolve it. Caught only by a forced
// clean build at merge time.
//
// A prototype existed (`/root/dep-declaration-check.mjs`, regex-based) and
// was correctly rejected: it caught the real defect (planting #800's bug
// moved its count 48 -> 49, named the file) but reported 27 FALSE POSITIVES
// on a clean tree, all indistinguishable in the output from the one real
// finding -- `@test/JsonBody` (a tsconfig path alias, not a package) and
// `, () => {` (the import regex matching a non-import string, almost
// certainly inside a test description). A check whose true positives are
// buried in its false ones is worse than no check, per the #873 standard
// this whole file exists to satisfy: ZERO false positives, not "few".
//
// WHY THE PROTOTYPE'S TWO DEFECTS BOTH TRACE TO THE SAME ROOT CAUSE: it
// classified specifiers with a regex-based `isLocal()` (only `./`, `@/`,
// `~`) instead of asking each package's OWN tsconfig whether a specifier
// resolves locally. `@test/JsonBody` is a real path alias this repo's
// tsconfigs declare; regex has no way to know that without hardcoding every
// alias prefix that will ever exist. Real module resolution does, because
// it is the SAME question `tsc` and the bundler already answer correctly.
//
// THE FIX: `ts.resolveModuleName` -- TypeScript's own resolver -- run
// against each package's OWN, ACTUAL parsed tsconfig (paths/baseUrl
// included), so a specifier is classified exactly the way that package's
// real build would resolve it: to a project file (same package, skip; a
// different workspace member, check its wire-name is declared), into
// node_modules (a third-party package, check it's declared), or
// unresolved (out of scope for this guard -- a broken import is a
// different defect class, already caught by typecheck). Imports are parsed
// via the real TypeScript AST (`ts.createSourceFile`), never a regex over
// source text, so `, () => {}` cannot masquerade as an import: it is
// syntactically not one, and the parser knows that in a way a `from\s*`
// pattern cannot.
//
// Root-hoisted devDependencies count as declared -- an established
// convention (already validated by the prototype's own 48->27 reduction),
// not a new relaxation.
//
// Run with `node --test scripts/test/dep-declaration.test.mjs`.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve, relative } from 'node:path'
import { tmpdir } from 'node:os'

import { skipSet } from "../tree-walk-lib.mjs";
import ts from 'typescript'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'))
}

/** Every workspace member (a directory matching the root package.json's
 *  `workspaces` globs that itself has a package.json), from source -- never
 *  a hardcoded list, so a new package is covered automatically. */
function workspaceMembers(root) {
  const rootPkg = readJson(join(root, 'package.json'))
  const members = []
  for (const glob of rootPkg.workspaces ?? []) {
    const base = String(glob).replace(/\/\*$/, '')
    const dir = join(root, base)
    if (!existsSync(dir)) continue
    for (const name of readdirSync(dir)) {
      const memberDir = join(base, name)
      if (existsSync(join(root, memberDir, 'package.json'))) members.push(memberDir)
    }
  }
  return members
}

const EXCLUDED_DIR_NAMES = skipSet()

function walkSourceFiles(dir, out = []) {
  let entries
  try {
    entries = readdirSync(dir)
  } catch {
    return out
  }
  for (const entry of entries) {
    if (EXCLUDED_DIR_NAMES.has(entry)) continue
    const full = join(dir, entry)
    let stat
    try {
      stat = statSync(full)
    } catch {
      continue
    }
    if (stat.isDirectory()) walkSourceFiles(full, out)
    else if (/\.(ts|tsx)$/.test(entry) && !/\.d\.ts$/.test(entry)) out.push(full)
  }
  return out
}

/** Every import/export/require specifier in a file, found via the REAL
 *  TypeScript AST -- never a regex over source text, so a string that only
 *  LOOKS like an import (a test description, a comment naming one) can
 *  never be misread as one, and neither can a construct a regex's author
 *  didn't anticipate. */
function importSpecifiers(fileName, sourceText) {
  const sourceFile = ts.createSourceFile(fileName, sourceText, ts.ScriptTarget.Latest, true)
  const specifiers = []
  function visit(node) {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      specifiers.push(node.moduleSpecifier.text)
    } else if (ts.isCallExpression(node)) {
      const isRequire = ts.isIdentifier(node.expression) && node.expression.text === 'require'
      const isDynamicImport = node.expression.kind === ts.SyntaxKind.ImportKeyword
      if ((isRequire || isDynamicImport) && node.arguments.length > 0 && ts.isStringLiteral(node.arguments[0])) {
        specifiers.push(node.arguments[0].text)
      }
    }
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  return specifiers
}

const NODE_BUILTINS = new Set([
  'assert', 'buffer', 'child_process', 'crypto', 'events', 'fs', 'http', 'https',
  'net', 'os', 'path', 'process', 'stream', 'timers', 'url', 'util', 'worker_threads', 'zlib'
])

function isBuiltin(specifier) {
  return specifier.startsWith('node:') || NODE_BUILTINS.has(specifier)
}

/** A config is SOLUTION-STYLE when it names no input files of its own and
 *  exists only to point at other projects via `references` -- e.g. every
 *  `apps/api`/`packages/{chiefing,piing,testing}` root tsconfig.json in this
 *  repo, each of which references its own `tsconfig.build.json` (src only)
 *  and `tsconfig.vitest.json` (src + test, with an ADDITIONAL `@test/*`
 *  path alias `tsconfig.build.json` does not have). Matches
 *  `assert-typecheck-nonvacuous.mjs`'s `isSolutionStyle` (same established
 *  instrument, not a second implementation of the same idea). */
function isSolutionStyleConfig(config) {
  const noInclude = !Array.isArray(config.include) || config.include.length === 0
  const noFiles = !Array.isArray(config.files) || config.files.length === 0
  const hasRefs = Array.isArray(config.references) && config.references.length > 0
  return noInclude && noFiles && hasRefs
}

/** Every LEAF (non-solution-style) tsconfig reachable from a member's root
 *  tsconfig, each with its OWN parsed compilerOptions and resolved file set
 *  -- e.g. `apps/api` resolves to BOTH `tsconfig.build.json` and
 *  `tsconfig.vitest.json`, and a specifier is classified against WHICHEVER
 *  leaf's file set actually contains the file doing the importing (test
 *  files need `tsconfig.vitest.json`'s `@test/*` alias; `tsconfig.build.json`
 *  does not have it and would misclassify it as external). A member with no
 *  tsconfig of its own (e.g. `packages/eslinter`, a pure-JS package) falls
 *  back to the repo-wide `tsconfig.base.json`. */
function memberLeafConfigs(root, memberDir) {
  const leaves = []
  const seen = new Set()
  function addLeaf(configPath) {
    const absConfigPath = resolve(configPath)
    if (seen.has(absConfigPath) || !existsSync(absConfigPath)) return
    seen.add(absConfigPath)
    const { config, error } = ts.readConfigFile(absConfigPath, ts.sys.readFile)
    if (error) {
      throw new Error(`cannot read ${absConfigPath}: ${ts.flattenDiagnosticMessageText(error.messageText, ' ')}`)
    }
    if (isSolutionStyleConfig(config)) {
      for (const reference of config.references ?? []) {
        const refPath = join(dirname(absConfigPath), reference.path)
        addLeaf(refPath.endsWith('.json') ? refPath : join(refPath, 'tsconfig.json'))
      }
      return
    }
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, dirname(absConfigPath))
    leaves.push({ options: parsed.options, fileNames: new Set(parsed.fileNames.map((f) => resolve(f))) })
  }
  addLeaf(join(root, memberDir, 'tsconfig.json'))
  if (leaves.length === 0) {
    const { config } = ts.readConfigFile(join(root, 'tsconfig.base.json'), ts.sys.readFile)
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, root)
    leaves.push({ options: parsed.options, fileNames: new Set() })
  }
  return leaves
}

/** Which leaf's options apply to a given file: the leaf whose OWN resolved
 *  file set actually contains it, falling back to the first leaf if none
 *  claim it (a file outside every tsconfig's include -- rare, but must not
 *  crash the scan; its resolution is best-effort in that case). */
function optionsForFile(leaves, fileAbs) {
  for (const leaf of leaves) {
    if (leaf.fileNames.has(fileAbs)) return leaf.options
  }
  return leaves[0].options
}

/** Resolve one specifier against the REAL compiler options that would
 *  actually apply to the file importing it, using TypeScript's own
 *  resolver -- the same question `tsc`/the bundler already answer
 *  correctly, asked directly instead of approximated with a prefix regex.
 *  Returns:
 *   - { kind: 'external', packageName }        -- resolved into node_modules
 *   - { kind: 'workspace', packageName, ownerMember } -- resolved to a real
 *     project file owned by a DIFFERENT workspace member
 *   - { kind: 'local' }                         -- resolved within the SAME
 *     member, or unresolved entirely (out of scope for this guard: an
 *     unresolvable import is a different defect class, already caught by
 *     typecheck -- see this file's own top-of-file note). */
function classifySpecifier(root, options, containingFile, specifier, memberDir, memberAbsToRel) {
  const result = ts.resolveModuleName(specifier, containingFile, options, ts.sys)
  const resolved = result.resolvedModule
  if (!resolved) return { kind: 'local' }

  if (resolved.isExternalLibraryImport) {
    const packageName = resolved.packageId?.name ?? packageNameFromNodeModulesPath(resolved.resolvedFileName)
    return packageName ? { kind: 'external', packageName } : { kind: 'local' }
  }

  const resolvedAbs = resolve(resolved.resolvedFileName)
  const ownerMember = ownerOfPath(resolvedAbs, memberAbsToRel)
  if (!ownerMember || ownerMember === memberDir) return { kind: 'local' }
  const ownerPkg = readJson(join(root, ownerMember, 'package.json'))
  return { kind: 'workspace', packageName: ownerPkg.name, ownerMember }
}

function packageNameFromNodeModulesPath(resolvedFileName) {
  const marker = '/node_modules/'
  const idx = resolvedFileName.lastIndexOf(marker)
  if (idx === -1) return undefined
  const afterNodeModules = resolvedFileName.slice(idx + marker.length)
  const segments = afterNodeModules.split('/')
  return segments[0].startsWith('@') ? `${segments[0]}/${segments[1]}` : segments[0]
}

/** Which workspace member (if any, as its ORIGINAL relative dir) a resolved
 *  absolute file path lives under. Longest-prefix match so a nested member
 *  path never matches its parent by accident (not a live shape in this
 *  repo, but cheap to get right). */
function ownerOfPath(absolutePath, memberAbsToRel) {
  let best
  for (const [memberAbs, memberRel] of memberAbsToRel) {
    if (absolutePath === memberAbs || absolutePath.startsWith(memberAbs + '/')) {
      if (!best || memberAbs.length > best.absLength) best = { rel: memberRel, absLength: memberAbs.length }
    }
  }
  return best?.rel
}

/** Every undeclared cross-package/third-party import across every
 *  workspace member, real resolution end to end. Shared by the real guard
 *  test and its tamper proof so both exercise the identical pipeline. */
function findUndeclaredImports(root) {
  const members = workspaceMembers(root)
  const memberAbsToRel = members.map((m) => [resolve(join(root, m)), m])
  const rootPkg = readJson(join(root, 'package.json'))
  const rootDeclared = new Set([
    ...Object.keys(rootPkg.dependencies ?? {}),
    ...Object.keys(rootPkg.devDependencies ?? {})
  ])

  const findings = []
  for (const memberDir of members) {
    const pkg = readJson(join(root, memberDir, 'package.json'))
    const declared = new Set([
      ...rootDeclared,
      ...Object.keys(pkg.dependencies ?? {}),
      ...Object.keys(pkg.devDependencies ?? {}),
      ...Object.keys(pkg.peerDependencies ?? {}),
      ...Object.keys(pkg.optionalDependencies ?? {})
    ])
    const leaves = memberLeafConfigs(root, memberDir)

    for (const file of walkSourceFiles(join(root, memberDir))) {
      const options = optionsForFile(leaves, resolve(file))
      const specifiers = importSpecifiers(file, readFileSync(file, 'utf8'))
      for (const specifier of specifiers) {
        if (isBuiltin(specifier)) continue
        const classification = classifySpecifier(root, options, file, specifier, memberDir, memberAbsToRel)
        if (classification.kind === 'local') continue
        if (!declared.has(classification.packageName)) {
          findings.push({
            member: memberDir,
            file: relative(root, file),
            specifier,
            packageName: classification.packageName,
            kind: classification.kind
          })
        }
      }
    }
  }
  return findings
}

// ---------------------------------------------------------------------------
// 1. The real guard: zero undeclared imports on the real, live tree.
// ---------------------------------------------------------------------------

test('every workspace member declares every package it imports (no undeclared cross-package/third-party import)', () => {
  const findings = findUndeclaredImports(repoRoot)
  assert.deepEqual(
    findings.map((f) => `${f.file}  imports '${f.specifier}' (package: ${f.packageName}, ${f.kind}) -- not declared in ${f.member}/package.json`),
    [],
    'an undeclared import resolves on a WARM bun install (a sibling workspace package, or a hoisted node_modules ' +
      'entry) and fails only on a CLEAN one -- CI and every fresh clone are clean trees (#800, #873)'
  )
})

test('sanity check: at least this many workspace members are scanned -- a near-zero count refuses to run rather than passing quietly (#848)', () => {
  const members = workspaceMembers(repoRoot)
  assert.ok(
    members.length >= 5,
    `only ${members.length} workspace member(s) found -- a workspaces glob probably broke (#848). REFUSING TO TRUST THIS RESULT.`
  )
})

// ---------------------------------------------------------------------------
// 2. Regression tests for the prototype's own two named false-positive
//    classes -- proving THIS guard does not reproduce either.
// ---------------------------------------------------------------------------

test('a tsconfig path alias (e.g. a repo test-helper alias) is resolved locally, never reported as an undeclared package', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-alias-'))
  try {
    writeFileSync(
      join(fixtureRoot, 'package.json'),
      JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'] })
    )
    const memberDir = join(fixtureRoot, 'packages', 'widget')
    mkdirSync(join(memberDir, 'src'), { recursive: true })
    mkdirSync(join(memberDir, 'test'), { recursive: true })
    writeFileSync(join(memberDir, 'package.json'), JSON.stringify({ name: '@fixture/widget' }))
    writeFileSync(
      join(memberDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { baseUrl: '.', paths: { '@test/*': ['./test/*'] }, moduleResolution: 'bundler' },
        include: ['src/**/*.ts', 'test/**/*.ts']
      })
    )
    writeFileSync(join(memberDir, 'test', 'JsonBody.ts'), 'export const fixture = 1\n')
    writeFileSync(
      join(memberDir, 'src', 'consumer.ts'),
      "import { fixture } from '@test/JsonBody'\nexport { fixture }\n"
    )

    const findings = findUndeclaredImports(fixtureRoot)
    assert.deepEqual(
      findings,
      [],
      "a real tsconfig path alias must resolve locally via ts.resolveModuleName, never surface as an undeclared " +
        "package -- this is the prototype's exact `@test/JsonBody` false positive"
    )
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('a non-import string that merely looks like one (a test description) is never treated as an import', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-nonimport-'))
  try {
    writeFileSync(
      join(fixtureRoot, 'package.json'),
      JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'] })
    )
    const memberDir = join(fixtureRoot, 'packages', 'widget')
    mkdirSync(join(memberDir, 'test'), { recursive: true })
    writeFileSync(join(memberDir, 'package.json'), JSON.stringify({ name: '@fixture/widget' }))
    writeFileSync(join(memberDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: {}, include: ['test/**/*.ts'] }))
    // The prototype's exact false positive: a regex matching `from\s*['"]`
    // or similar inside a string that is not an import at all -- a test
    // description reading "..., () => {" is the reported shape.
    writeFileSync(
      join(memberDir, 'test', 'Widget.test.ts'),
      "const description = 'from \"not-a-real-package\", () => {'\nconsole.log(description)\n"
    )

    const findings = findUndeclaredImports(fixtureRoot)
    assert.deepEqual(
      findings,
      [],
      'a string that merely contains import-shaped text must never be parsed as an import -- real AST parsing ' +
        'cannot mistake a string literal for an ImportDeclaration'
    )
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('a root-hoisted devDependency counts as declared for every member (established convention, not a new relaxation)', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-roothoist-'))
  try {
    writeFileSync(
      join(fixtureRoot, 'package.json'),
      JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'], devDependencies: { 'hoisted-tool': '1.0.0' } })
    )
    const memberDir = join(fixtureRoot, 'packages', 'widget')
    mkdirSync(join(memberDir, 'src'), { recursive: true })
    mkdirSync(join(fixtureRoot, 'node_modules', 'hoisted-tool'), { recursive: true })
    writeFileSync(join(fixtureRoot, 'node_modules', 'hoisted-tool', 'package.json'), JSON.stringify({ name: 'hoisted-tool', main: 'index.js' }))
    writeFileSync(join(fixtureRoot, 'node_modules', 'hoisted-tool', 'index.js'), 'module.exports = {}\n')
    writeFileSync(join(fixtureRoot, 'node_modules', 'hoisted-tool', 'index.d.ts'), 'export {}\n')
    writeFileSync(join(memberDir, 'package.json'), JSON.stringify({ name: '@fixture/widget' }))
    writeFileSync(join(memberDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { moduleResolution: 'node' }, include: ['src/**/*.ts'] }))
    writeFileSync(join(memberDir, 'src', 'consumer.ts'), "import 'hoisted-tool'\n")

    const findings = findUndeclaredImports(fixtureRoot)
    assert.deepEqual(findings, [], 'a root-hoisted devDependency must count as declared for every member')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// ---------------------------------------------------------------------------
// 3. THE #800 REGRESSION, reproduced structurally: an undeclared workspace
//    cross-package import is caught, red then green.
// ---------------------------------------------------------------------------

test('#800\'s exact defect: an undeclared workspace cross-package import is caught, and clears once declared -- demonstrated red then green', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-800-'))
  try {
    writeFileSync(
      join(fixtureRoot, 'package.json'),
      JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'] })
    )
    const producerDir = join(fixtureRoot, 'packages', 'producer')
    mkdirSync(join(producerDir, 'src'), { recursive: true })
    writeFileSync(join(producerDir, 'package.json'), JSON.stringify({ name: '@fixture/producer', main: 'src/index.ts' }))
    writeFileSync(join(producerDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: {}, include: ['src/**/*.ts'] }))
    writeFileSync(join(producerDir, 'src', 'index.ts'), 'export const widget = 1\n')

    const consumerDir = join(fixtureRoot, 'packages', 'consumer')
    mkdirSync(join(consumerDir, 'src'), { recursive: true })
    // Deliberately declares NOTHING -- the exact #800 shape: apps/api's
    // package.json declared @chief/chiefing but not the @chief/piing it
    // actually imported.
    writeFileSync(join(consumerDir, 'package.json'), JSON.stringify({ name: '@fixture/consumer', dependencies: {} }))
    writeFileSync(
      join(consumerDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { paths: { '@fixture/producer': ['../producer/src/index.ts'] } },
        include: ['src/**/*.ts']
      })
    )
    writeFileSync(join(consumerDir, 'src', 'Consumer.ts'), "import { widget } from '@fixture/producer'\nexport { widget }\n")

    // RED: undeclared.
    const redFindings = findUndeclaredImports(fixtureRoot)
    assert.equal(redFindings.length, 1, `expected exactly one undeclared import, found ${JSON.stringify(redFindings)}`)
    assert.equal(redFindings[0].packageName, '@fixture/producer')
    assert.equal(redFindings[0].kind, 'workspace')
    assert.match(redFindings[0].file, /Consumer\.ts$/)

    // GREEN: declare it, same fixture, same pipeline.
    writeFileSync(
      join(consumerDir, 'package.json'),
      JSON.stringify({ name: '@fixture/consumer', dependencies: { '@fixture/producer': 'workspace:*' } })
    )
    const greenFindings = findUndeclaredImports(fixtureRoot)
    assert.deepEqual(greenFindings, [], 'declaring the dependency must clear the finding against the identical pipeline')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('an undeclared THIRD-PARTY (node_modules) import is caught the same way a workspace one is', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-thirdparty-'))
  try {
    writeFileSync(join(fixtureRoot, 'package.json'), JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'] }))
    mkdirSync(join(fixtureRoot, 'node_modules', 'left-pad'), { recursive: true })
    writeFileSync(join(fixtureRoot, 'node_modules', 'left-pad', 'package.json'), JSON.stringify({ name: 'left-pad', main: 'index.js' }))
    writeFileSync(join(fixtureRoot, 'node_modules', 'left-pad', 'index.js'), 'module.exports = {}\n')
    writeFileSync(join(fixtureRoot, 'node_modules', 'left-pad', 'index.d.ts'), 'export {}\n')

    const memberDir = join(fixtureRoot, 'packages', 'widget')
    mkdirSync(join(memberDir, 'src'), { recursive: true })
    writeFileSync(join(memberDir, 'package.json'), JSON.stringify({ name: '@fixture/widget', dependencies: {} }))
    writeFileSync(join(memberDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { moduleResolution: 'node' }, include: ['src/**/*.ts'] }))
    writeFileSync(join(memberDir, 'src', 'consumer.ts'), "import 'left-pad'\n")

    const findings = findUndeclaredImports(fixtureRoot)
    assert.equal(findings.length, 1)
    assert.equal(findings[0].packageName, 'left-pad')
    assert.equal(findings[0].kind, 'external')
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

test('an unresolvable specifier (out of this guard\'s scope -- a broken import, not an undeclared one) is never reported', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'dep-declaration-unresolved-'))
  try {
    writeFileSync(join(fixtureRoot, 'package.json'), JSON.stringify({ name: 'fixture-root', workspaces: ['packages/*'] }))
    const memberDir = join(fixtureRoot, 'packages', 'widget')
    mkdirSync(join(memberDir, 'src'), { recursive: true })
    writeFileSync(join(memberDir, 'package.json'), JSON.stringify({ name: '@fixture/widget' }))
    writeFileSync(join(memberDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: {}, include: ['src/**/*.ts'] }))
    writeFileSync(join(memberDir, 'src', 'consumer.ts'), "import { x } from 'this-package-does-not-exist-anywhere'\nexport { x }\n")

    const findings = findUndeclaredImports(fixtureRoot)
    assert.deepEqual(
      findings,
      [],
      'an unresolvable import is a DIFFERENT defect class (already caught by typecheck), not this guard\'s job -- ' +
        'reporting it here would be exactly the kind of unexplained hit that destroyed the prototype\'s signal'
    )
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})

// Guard for knip.jsonc's workspace map integrity (#845).
//
// Adjacent workspace blocks in knip.jsonc share a trailing entry/project pair
// and closing brace. When two stories each append a block, git resolves the
// overlap CLEANLY — no conflict markers, no failing gate — into a SINGLE
// block carrying one package's content under another package's key. `knip`
// itself still exits 0 (the spliced block is syntactically valid config),
// typecheck is unaffected, and the conflict-marker sweep finds nothing: the
// failure is invisible to every other check in the repo. This has happened
// on three consecutive merges, caught only because the merger rebuilt the
// blocks by hand and counted them.
//
// Two independent checks, because a splice can go wrong in two different
// ways:
//   1. Key-set check: the set of keys in knip.jsonc's workspace map must
//      exactly equal the set of workspace members resolved from the root
//      package.json's `workspaces` globs (plus the fixed "." root entry).
//      Catches a splice that drops or duplicates a KEY.
//   2. Path-ownership check: every entry/project path declared under a
//      workspace key must resolve to a real file/directory UNDER THAT
//      WORKSPACE'S OWN DIRECTORY. Catches the splice team-lead described
//      directly: the key count stays right, but one block's paths (e.g.
//      packages/piing's `src/extensionruntime/index.ts`) end up sitting
//      under a different package's key (e.g. apps/web) where no such path
//      exists on disk.
//
// Run with `node --test scripts/test/knip-workspace-map.test.mjs`.
//
// Mandate 1 (reactive-only) note: every check here is a single synchronous
// read — no polling, no interval, no sleep.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve } from 'node:path'
import ts from 'typescript'

// Read JSONC with TYPESCRIPT'S OWN parser, not a hand-rolled comment strip —
// same reasoning and same instrument `assert-typecheck-nonvacuous.mjs`'s
// `readTsconfig` uses for tsconfig files: a regex-based `//`-strip is a
// fragile parser guarding against fragile-parser risk, and defers to the
// compiler that owns the format instead. #878 moved this guard's target
// from knip.json to knip.jsonc specifically so adjacent workspace blocks
// could carry a disambiguating trailing comment naming the package — real
// JSONC, not a hack, and knip itself recognizes `knip.jsonc` as a config
// filename (checked in `node_modules/knip/dist/constants.js`, ahead of
// plain `knip.json` in its own lookup order).
function readJsonc(path) {
  const absolute = resolve(path)
  const { config, error } = ts.readConfigFile(absolute, ts.sys.readFile)
  if (error) {
    throw new Error(`cannot read ${absolute}: ${ts.flattenDiagnosticMessageText(error.messageText, ' ')}`)
  }
  return config ?? {}
}

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(__dirname, '..', '..')

function repoFile(root, ...segments) {
  return join(root, ...segments)
}

// Every directory matched by a `workspaces` glob of the form "<dir>/*" that
// itself contains a package.json — the same definition bun/npm workspaces
// use. Returns keys in the exact shape knip.jsonc's workspace map uses
// ("apps/web", "packages/chiefing", ...), not absolute paths.
function resolveWorkspaceMemberKeys(root, globs) {
  const keys = []
  for (const glob of globs) {
    if (!glob.endsWith('/*')) {
      throw new Error(`[knip-workspace-map] unsupported workspaces glob "${glob}" — only "<dir>/*" is handled`)
    }
    const parentRel = glob.slice(0, -2)
    const parentAbs = repoFile(root, parentRel)
    if (!existsSync(parentAbs)) continue
    for (const entry of readdirSync(parentAbs, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue
      const memberRel = `${parentRel}/${entry.name}`
      if (existsSync(repoFile(root, memberRel, 'package.json'))) {
        keys.push(memberRel)
      }
    }
  }
  return keys
}

// The static (non-glob) prefix of a knip entry/project pattern, relative to
// the workspace directory it is declared under. A literal path
// ("src/index.ts") must resolve to a FILE; a glob ("src/**/*.ts",
// "src/app/**/*.{ts,tsx}") resolves to the directory portion before the
// first wildcard character, which must exist as a DIRECTORY. A pattern with
// no static directory prefix at all ("**/*.js", "*.ts") carries nothing this
// check can verify and is skipped — every real workspace block in this repo
// mixes such patterns with at least one literal or prefixed-glob entry, so
// skipping the unverifiable ones does not blind the guard.
function staticPrefixOf(pattern) {
  const specialIndex = pattern.search(/[*{}?[\]]/)
  if (specialIndex === -1) {
    return { relPath: pattern, kind: 'file' }
  }
  const beforeSpecial = pattern.slice(0, specialIndex)
  const lastSlash = beforeSpecial.lastIndexOf('/')
  if (lastSlash === -1) {
    return { relPath: '', kind: 'unverifiable' }
  }
  return { relPath: beforeSpecial.slice(0, lastSlash), kind: 'dir' }
}

// ---------------------------------------------------------------------------
// The validator. Pure function of (repoRoot, knipConfig, workspacesGlobs) ->
// string[] of violation messages, so it can be exercised against both the
// real config (for the guard) and a doctored fixture (for the negative
// self-tests) without duplicating logic.
// ---------------------------------------------------------------------------
export function validateKnipWorkspaceMap(root, knip, workspacesGlobs) {
  const errors = []
  const declaredKeys = Object.keys(knip?.workspaces ?? {})
  const resolvedMembers = resolveWorkspaceMemberKeys(root, workspacesGlobs)
  const expectedKeys = new Set(['.', ...resolvedMembers])
  const declaredSet = new Set(declaredKeys)

  for (const expected of expectedKeys) {
    if (!declaredSet.has(expected)) {
      errors.push(`knip.jsonc's workspace map is missing key "${expected}" (resolved from the root workspaces globs)`)
    }
  }
  for (const declared of declaredSet) {
    if (!expectedKeys.has(declared)) {
      errors.push(`knip.jsonc's workspace map has key "${declared}" which is not a resolved workspace member`)
    }
  }

  for (const key of declaredKeys) {
    if (key === '.') continue
    const block = knip.workspaces[key];
    const baseDir = repoFile(root, key)
    for (const field of ['entry', 'project']) {
      const patterns = Array.isArray(block?.[field]) ? block[field] : []
      for (const pattern of patterns) {
        const { relPath, kind } = staticPrefixOf(pattern)
        if (kind === 'unverifiable') continue
        const candidate = relPath === '' ? baseDir : join(baseDir, relPath)
        if (!existsSync(candidate)) {
          errors.push(
            `knip.jsonc's "${key}".${field} entry "${pattern}" resolves to ${key}/${relPath || '.'}, which does not exist — this workspace's paths may have been spliced under the wrong key`,
          )
          continue
        }
        if (kind === 'file' && !statSync(candidate).isFile()) {
          errors.push(`knip.jsonc's "${key}".${field} entry "${pattern}" resolves to a path that is not a file`)
        }
        if (kind === 'dir' && !statSync(candidate).isDirectory()) {
          errors.push(`knip.jsonc's "${key}".${field} entry "${pattern}" resolves to a path that is not a directory`)
        }
      }
    }
  }

  return errors
}

// A package export is a consumer-facing entry point even when no source file
// inside this repository imports it. Knip cannot infer that a published
// `dist/...` subpath is intentionally live, so the source counterpart must be
// declared explicitly in the owning workspace's `entry` list. Keep this
// derivation tied to package metadata instead of repeating a handwritten
// source path: changing the public export must either keep the mapping valid
// or make this guard explain the missing configuration.
export function validateChiefingExtensionRuntimeKnipEntry(root, knip) {
  const errors = []
  const workspaceKey = 'packages/chiefing'
  const manifestPath = repoFile(root, workspaceKey, 'package.json')
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'))
  const exportTarget = manifest.exports?.['./extension-runtime']?.import

  if (typeof exportTarget !== 'string') {
    errors.push(`${workspaceKey}/package.json must expose an import target for ./extension-runtime`)
    return errors
  }

  const distMatch = /^\.\/dist\/(.+)\.js$/.exec(exportTarget)
  if (!distMatch) {
    errors.push(
      `${workspaceKey}/package.json export ./extension-runtime import target ${JSON.stringify(exportTarget)} cannot map from dist to a TypeScript source entry`,
    )
    return errors
  }

  const sourceEntry = `src/${distMatch[1]}.ts`
  if (!existsSync(repoFile(root, workspaceKey, sourceEntry))) {
    errors.push(`${workspaceKey}/package.json export ./extension-runtime maps to missing source entry ${sourceEntry}`)
    return errors
  }

  const entries = knip?.workspaces?.[workspaceKey]?.entry
  if (!Array.isArray(entries) || !entries.includes(sourceEntry)) {
    errors.push(
      `knip.jsonc must declare ${workspaceKey}/${sourceEntry} as an explicit entry for package export ./extension-runtime`,
    )
  }

  return errors
}

export function validatePiingRuntimeExtensionKnipGraph(root, knip) {
  const errors = []
  const workspaceKey = 'packages/piing'
  const extensionPattern = 'extensions/*.ts'
  const extensionRoot = repoFile(root, workspaceKey, 'extensions')
  const extensionFiles = readdirSync(extensionRoot).filter((entry) => entry.endsWith('.ts'))

  if (extensionFiles.length === 0) {
    errors.push(`${workspaceKey}/extensions must contain runtime TypeScript entries`)
  }
  for (const field of ['entry', 'project']) {
    const patterns = knip?.workspaces?.[workspaceKey]?.[field]
    if (!Array.isArray(patterns) || !patterns.includes(extensionPattern)) {
      errors.push(`knip.jsonc must include ${workspaceKey}/${extensionPattern} in ${field}`)
    }
  }
  return errors
}

// #809 is intentionally ahead of its isolated S5/S6/S7 UI consumers. The
// private apps/web package has neither npm exports nor a barrel: putting a
// fake provider in the app tree just to satisfy Knip would change ownership
// and runtime behavior. Instead, its package metadata records the four real
// consumer contracts while they remain deferred. This is NOT a package export
// mechanism or runtime wiring; it is the narrow source of truth from which
// Knip's temporary literal roots are derived.
const PRIVATE_WEB_SSE_CONSUMER_ROLES = [
  'app-shell',
  'S5-lifecycle',
  'S6-person',
  'S7-company'
]

function hasOwn(object, key) {
  return Object.prototype.hasOwnProperty.call(object, key)
}

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function hasGlobMagic(value) {
  return /[*?{}\[\]]/.test(value)
}

function isWebSourceEntry(value) {
  return (
    typeof value === 'string' &&
    /^src\/(?:[A-Za-z0-9_.-]+\/)*[A-Za-z0-9_.-]+\.tsx?$/.test(value) &&
    !value.includes('..') &&
    !hasGlobMagic(value)
  )
}

function hasExportModifier(statement) {
  return statement.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword) ?? false
}

// Parse the real TypeScript source rather than scanning it with a regex. A
// comment, a local declaration, or a syntactically broken file must never be
// mistaken for the documented public symbol.
function inspectNamedSourceExport(path, contents, exportName) {
  // `parseDiagnostics` is real and populated, but TypeScript declares it on
  // the internal `SourceFile` shape rather than the public one.
  const sourceFile = /** @type {ts.SourceFile & { parseDiagnostics: ts.Diagnostic[] }} */ (
    ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true)
  )
  if (sourceFile.parseDiagnostics.length > 0) {
    return {
      parses: false,
      exportsName: false,
      diagnostic: ts.flattenDiagnosticMessageText(sourceFile.parseDiagnostics[0].messageText, ' ')
    }
  }

  for (const statement of sourceFile.statements) {
    if (
      hasExportModifier(statement) &&
      (ts.isFunctionDeclaration(statement) ||
        ts.isClassDeclaration(statement) ||
        ts.isInterfaceDeclaration(statement) ||
        ts.isEnumDeclaration(statement) ||
        ts.isTypeAliasDeclaration(statement)) &&
      statement.name?.text === exportName
    ) {
      return { parses: true, exportsName: true }
    }
    if (hasExportModifier(statement) && ts.isVariableStatement(statement)) {
      if (
        statement.declarationList.declarations.some(
          (declaration) => ts.isIdentifier(declaration.name) && declaration.name.text === exportName
        )
      ) {
        return { parses: true, exportsName: true }
      }
    }
    if (
      ts.isExportDeclaration(statement) &&
      statement.exportClause &&
      ts.isNamedExports(statement.exportClause)
    ) {
      if (statement.exportClause.elements.some((element) => element.name.text === exportName)) {
        return { parses: true, exportsName: true }
      }
    }
  }

  return { parses: true, exportsName: false }
}

function readWebPackage(root) {
  return JSON.parse(readFileSync(repoFile(root, 'apps', 'web', 'package.json'), 'utf8'))
}

// Returns metadata and diagnostics separately so the Knip validator can be
// exercised against in-memory stale-path/export fixtures without writing to
// the worktree.
export function readPrivateWebSseConsumerContracts(root, webPackage) {
  const errors = []
  const contracts = []
  const rolesByEntry = new Map()
  const packageLabel = 'apps/web/package.json'

  if (!isRecord(webPackage)) {
    return { contracts, errors: [`${packageLabel} must parse to an object`] }
  }
  if (webPackage.private !== true) {
    errors.push(`${packageLabel} must remain private while its deferred consumer contracts are active`)
  }
  if (hasOwn(webPackage, 'exports')) {
    errors.push(`${packageLabel} must not add npm exports for private deferred SSE consumer contracts`)
  }

  const definitions = webPackage.chief?.privateWebDeferredConsumerContracts?.sse
  if (!isRecord(definitions)) {
    return {
      contracts,
      errors: [
        ...errors,
        `${packageLabel} must define chief.privateWebDeferredConsumerContracts.sse as an object`
      ]
    }
  }

  for (const role of PRIVATE_WEB_SSE_CONSUMER_ROLES) {
    if (!hasOwn(definitions, role)) {
      errors.push(`${packageLabel} is missing deferred SSE consumer contract "${role}"`)
    }
  }
  for (const role of Object.keys(definitions)) {
    if (!PRIVATE_WEB_SSE_CONSUMER_ROLES.includes(role)) {
      errors.push(`${packageLabel} has stale deferred SSE consumer contract "${role}"`)
    }
  }

  for (const role of PRIVATE_WEB_SSE_CONSUMER_ROLES) {
    const contract = definitions[role]
    if (!isRecord(contract)) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" must be an object with one literal entry and one exported identifier`
      )
      continue
    }
    const entry = contract.entry
    const exportName = contract.export
    const unexpectedFields = Object.keys(contract).filter((key) => key !== 'entry' && key !== 'export')
    if (unexpectedFields.length > 0) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" has stale field(s): ${unexpectedFields.join(', ')}`
      )
    }
    if (!isWebSourceEntry(entry)) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" must name one literal apps/web source entry, got ${JSON.stringify(entry)}`
      )
      continue
    }
    if (typeof exportName !== 'string' || !/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(exportName)) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" must name one exported identifier, got ${JSON.stringify(exportName)}`
      )
      continue
    }

    const sourcePath = repoFile(root, 'apps', 'web', entry)
    if (!existsSync(sourcePath) || !statSync(sourcePath).isFile()) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" has stale path ${entry}; the source file does not exist`
      )
      continue
    }
    const inspection = inspectNamedSourceExport(sourcePath, readFileSync(sourcePath, 'utf8'), exportName)
    if (!inspection.parses) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" points at unparsable source ${entry}: ${inspection.diagnostic}`
      )
      continue
    }
    if (!inspection.exportsName) {
      errors.push(
        `${packageLabel}'s deferred SSE contract "${role}" has stale export ${exportName} in ${entry}`
      )
      continue
    }
    const priorRole = rolesByEntry.get(entry)
    if (priorRole) {
      errors.push(
        `${packageLabel}'s deferred SSE contracts "${priorRole}" and "${role}" both name ${entry}; each consumer needs its own root`
      )
      continue
    }
    rolesByEntry.set(entry, role)
    contracts.push({ role, entry, exportName })
  }

  if (contracts.length !== PRIVATE_WEB_SSE_CONSUMER_ROLES.length) {
    errors.push(
      `${packageLabel} must resolve exactly ${PRIVATE_WEB_SSE_CONSUMER_ROLES.length} valid private-web SSE consumer contracts, found ${contracts.length}`
    )
  }

  return { contracts, errors }
}

function escapeRegexLiteral(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

// A deliberately small, deterministic glob matcher for the Knip patterns we
// need to reject here. It supports ordinary stars, recursive stars, question
// marks, and extension braces; unsupported braces remain literal rather than
// accidentally blessing a suppression pattern.
function globMatchesPath(pattern, path) {
  if (typeof pattern !== 'string') return false
  let expression = ''
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index]
    if (character === '*') {
      if (pattern[index + 1] === '*') {
        while (pattern[index + 1] === '*') index += 1
        if (pattern[index + 1] === '/') {
          expression += '(?:.*/)?'
          index += 1
        } else {
          expression += '.*'
        }
      } else {
        expression += '[^/]*'
      }
      continue
    }
    if (character === '?') {
      expression += '[^/]'
      continue
    }
    if (character === '{') {
      const close = pattern.indexOf('}', index + 1)
      const alternatives = close === -1 ? [] : pattern.slice(index + 1, close).split(',')
      if (alternatives.length > 0 && alternatives.every((alternative) => /^[^*?{}\[\]/]+$/.test(alternative))) {
        expression += `(?:${alternatives.map(escapeRegexLiteral).join('|')})`
        index = close
        continue
      }
    }
    expression += escapeRegexLiteral(character)
  }
  return new RegExp(`^${expression}$`).test(path)
}

function isPrivateWebDeferredSourceEntry(entry) {
  return typeof entry === 'string' && /^src\/(?:hooks|providers|services)\//.test(entry)
}

function matchingIgnoredEntries(patterns, path) {
  if (!Array.isArray(patterns)) return []
  return patterns.filter((pattern) => globMatchesPath(pattern, path))
}

// The four literal entries MUST be derived from the metadata above. A broad
// glob would hide an orphan in the same directory, and an ignore would merely
// silence the signal, so both are deliberately red states even if Knip exits
// successfully after applying them.
export function validatePrivateWebSseConsumerKnipEntries(root, knip, webPackage) {
  const { contracts, errors } = readPrivateWebSseConsumerContracts(root, webPackage)
  const webWorkspace = knip?.workspaces?.['apps/web']
  const entries = webWorkspace?.entry
  if (!Array.isArray(entries)) {
    errors.push(`knip.jsonc's apps/web workspace must declare an entry array for private SSE consumers`)
    return errors
  }

  const expectedEntries = new Set(contracts.map((contract) => contract.entry))
  for (const contract of contracts) {
    const occurrences = entries.filter((entry) => entry === contract.entry).length
    if (occurrences === 0) {
      errors.push(
        `knip.jsonc must declare apps/web/${contract.entry} as a literal entry for deferred ${contract.role} consumer ${contract.exportName}`
      )
    } else if (occurrences > 1) {
      errors.push(`knip.jsonc declares apps/web/${contract.entry} more than once for deferred ${contract.role}`)
    }
  }

  for (const entry of entries) {
    if (typeof entry !== 'string') continue
    for (const contract of contracts) {
      if (hasGlobMagic(entry) && globMatchesPath(entry, contract.entry)) {
        errors.push(
          `knip.jsonc's apps/web entry pattern ${JSON.stringify(entry)} broadly covers deferred ${contract.role}; use literal ${contract.entry}`
        )
      }
    }
    if (isPrivateWebDeferredSourceEntry(entry) && !expectedEntries.has(entry)) {
      errors.push(
        `knip.jsonc has stale private-web deferred source entry ${entry}; add its real consumer metadata or remove it`
      )
    }
  }

  for (const contract of contracts) {
    const rootMatches = matchingIgnoredEntries(knip?.ignore, `apps/web/${contract.entry}`)
    const workspaceMatches = matchingIgnoredEntries(webWorkspace?.ignore, contract.entry)
    for (const pattern of [...rootMatches, ...workspaceMatches]) {
      errors.push(
        `knip.jsonc ignores deferred ${contract.role} entry ${contract.entry} via ${JSON.stringify(pattern)}; declare the literal entry instead`
      )
    }
  }

  return errors
}

// ---------------------------------------------------------------------------
// apps/web's UNIT SUITE MUST BE PART OF THE GRAPH (#751 knip sweep).
//
// knip.jsonc used to carry a blanket `ignore: ["**/test/**", ...]`. That one
// row removed every test file from the dependency graph, so an export whose
// ONLY consumer is a unit test read as an unused export. The report named
// eleven of them at once -- `hostedPeople`, `translateFeedFrame`,
// `personStreamFrame`, `HEARTBEAT_MS`, `departmentOf`, `contrastRatio`,
// `relativeLuminance`, `CHIP_INK_LIGHT`, `CHIP_INK_DARK`,
// `ORG_STORE_BASE_STORES`, `useCompanyDirectoryWithClient` -- all live and
// all covered. A lexical instrument cannot tell dead code from code reached
// another way, and deleting on that report would have deleted the covered
// code AND its coverage.
//
// The two wrong repairs are both blocked here. Re-ignoring the tree brings
// the false positives back; adding an `ignoreExportsUsedInFile`/ignore row
// for the eleven names silences the true positives too and leaves a stale
// allowlist behind the first time one of them is genuinely deleted. The
// right repair is the one this guard pins: the test tree is an ENTRY POINT
// and a PROJECT source, and the `@test/*` alias -- which lives only in
// `apps/web/vitest.config.ts`, because the app tsconfig excludes `test` --
// is restated in knip's own `paths` so an `@test/...` import resolves to a
// file instead of reading as an unlisted npm dependency.
//
// Every fact below is derived from disk: a REAL test file the suite runs and
// a REAL `@test/...` importer, so the guard cannot pass vacuously against an
// empty or moved test tree.
// ---------------------------------------------------------------------------

// A real `apps/web/test/**/*.test.{ts,tsx}` file, repo-relative, or null when
// the tree is empty (which is itself a red state below).
function findWebTestFile(root) {
  const stack = [[]]
  while (stack.length > 0) {
    const segments = stack.pop()
    const absolute = repoFile(root, 'apps', 'web', 'test', ...segments)
    if (!existsSync(absolute)) continue
    for (const item of readdirSync(absolute, { withFileTypes: true })) {
      if (item.isDirectory()) {
        stack.push([...segments, item.name])
      } else if (/\.test\.tsx?$/.test(item.name)) {
        return ['test', ...segments, item.name].join('/')
      }
    }
  }
  return null
}

// The `@test/*` alias exists in exactly one place a bundler reads --
// `apps/web/vitest.config.ts` -- so that file, not a remembered string, is
// what knip's `paths` is checked against.
function webVitestAliasesTestDirectory(root) {
  const source = readFileSync(repoFile(root, 'apps', 'web', 'vitest.config.ts'), 'utf8')
  return /'@test':\s*path\.resolve\(dirname,\s*'\.\/test'\)/.test(source)
}

export function validateWebTestGraphVisibility(root, knip, webTestFile) {
  const errors = []
  const web = knip?.workspaces?.['apps/web']
  const label = "knip.jsonc's apps/web"

  if (!webTestFile) {
    errors.push('apps/web/test carries no *.test.ts(x) file — this guard would pass vacuously')
    return errors
  }

  for (const pattern of matchingIgnoredEntries(knip?.ignore, `apps/web/${webTestFile}`)) {
    errors.push(
      `knip.jsonc's top-level ignore hides apps/web/${webTestFile} via ${JSON.stringify(pattern)}; the unit suite must stay in the graph so a test-only export is not reported as dead`
    )
  }
  for (const pattern of matchingIgnoredEntries(web?.ignore, webTestFile)) {
    errors.push(
      `${label} ignore hides ${webTestFile} via ${JSON.stringify(pattern)}; the unit suite must stay in the graph so a test-only export is not reported as dead`
    )
  }

  for (const field of ['entry', 'project']) {
    const patterns = web?.[field]
    if (!Array.isArray(patterns)) {
      errors.push(`${label} must declare a ${field} array`)
      continue
    }
    if (!patterns.some((pattern) => globMatchesPath(pattern, webTestFile))) {
      errors.push(`${label} ${field} must cover ${webTestFile} — the unit suite is a root of the graph, not noise`)
    }
  }

  if (!webVitestAliasesTestDirectory(root)) {
    errors.push("apps/web/vitest.config.ts no longer aliases '@test' to './test'; knip's paths row now describes nothing")
  }
  const aliased = web?.paths?.['@test/*']
  if (!Array.isArray(aliased) || !aliased.includes('./test/*')) {
    errors.push(
      `${label} paths must map "@test/*" to "./test/*" — vitest.config.ts owns that alias and the app tsconfig excludes test, so knip reads every @test/... import as an unlisted dependency without it`
    )
  }

  return errors
}

// ---------------------------------------------------------------------------
// Guard tests against the real config.
// ---------------------------------------------------------------------------

const knipPath = repoFile(repoRoot, 'knip.jsonc')
const packageJsonPath = repoFile(repoRoot, 'package.json')
const knip = readJsonc(knipPath)
const rootManifest = JSON.parse(readFileSync(packageJsonPath, 'utf8'))
const workspacesGlobs = rootManifest.workspaces
const webPackage = readWebPackage(repoRoot)

test('knip.jsonc exists and parses (JSONC, comments and all)', () => {
  assert.ok(existsSync(knipPath))
  assert.ok(statSync(knipPath).size > 0)
  assert.ok(Object.keys(knip.workspaces ?? {}).length > 0, 'must actually parse to real workspace keys, not an empty object from a swallowed parse error')
})

test('a plain knip.json no longer exists alongside knip.jsonc (knip checks knip.json first in its lookup order, so a stale leftover would silently win)', () => {
  assert.equal(existsSync(repoFile(repoRoot, 'knip.json')), false)
})

test('root package.json declares workspaces globs this guard can resolve', () => {
  assert.ok(Array.isArray(workspacesGlobs) && workspacesGlobs.length > 0)
})

test('knip.jsonc workspace map keys exactly match the resolved workspace members (plus ".")', () => {
  const errors = validateKnipWorkspaceMap(repoRoot, knip, workspacesGlobs).filter(
    (m) => m.includes('is missing key') || m.includes('is not a resolved workspace member'),
  )
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('every workspace block\'s entry/project paths resolve under its own key (no spliced block)', () => {
  const errors = validateKnipWorkspaceMap(repoRoot, knip, workspacesGlobs).filter((m) =>
    m.includes('resolves to'),
  )
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('Chiefing public extension-runtime export is an explicit Knip entry', () => {
  const errors = validateChiefingExtensionRuntimeKnipEntry(repoRoot, knip)
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('Piing runtime extensions are non-empty Knip entries and project sources', () => {
  const errors = validatePiingRuntimeExtensionKnipGraph(repoRoot, knip)
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('private web SSE deferred consumer contracts derive exact literal Knip entries', () => {
  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, knip, webPackage)
  assert.deepEqual(errors, [], errors.join('\n'))
})

const webTestFile = findWebTestFile(repoRoot)

test("apps/web's unit suite is in the knip graph (a test-only export is not a dead export)", () => {
  assert.ok(webTestFile, 'expected a real apps/web/test/**/*.test.ts(x) file to check against')
  const errors = validateWebTestGraphVisibility(repoRoot, knip, webTestFile)
  assert.deepEqual(errors, [], errors.join('\n'))
})

test('RED: re-ignoring the web test tree fails — that ignore is what made eleven live exports read as dead', () => {
  const ignored = { ...knip, ignore: ['**/test/**', ...(knip.ignore ?? [])] }
  const errors = validateWebTestGraphVisibility(repoRoot, ignored, webTestFile)
  assert.ok(
    errors.some((message) => message.includes('top-level ignore hides')),
    errors.join('\n')
  )
})

test('RED: a workspace-level ignore is the same mistake and fails the same way', () => {
  const ignored = {
    ...knip,
    workspaces: {
      ...knip.workspaces,
      'apps/web': { ...knip.workspaces['apps/web'], ignore: ['test/**'] }
    }
  }
  const errors = validateWebTestGraphVisibility(repoRoot, ignored, webTestFile)
  assert.ok(
    errors.some((message) => message.includes('ignore hides')),
    errors.join('\n')
  )
})

test('RED: dropping the test tree from entry or project fails, one message per field', () => {
  const stripped = {
    ...knip,
    workspaces: {
      ...knip.workspaces,
      'apps/web': {
        ...knip.workspaces['apps/web'],
        entry: knip.workspaces['apps/web'].entry.filter((pattern) => !pattern.startsWith('test/')),
        project: knip.workspaces['apps/web'].project.filter((pattern) => !pattern.startsWith('test/'))
      }
    }
  }
  const errors = validateWebTestGraphVisibility(repoRoot, stripped, webTestFile)
  assert.ok(errors.some((message) => message.includes('entry must cover')), errors.join('\n'))
  assert.ok(errors.some((message) => message.includes('project must cover')), errors.join('\n'))
})

test('RED: dropping the @test/* alias fails — every @test/... import would read as an unlisted dependency', () => {
  const web = { ...knip.workspaces['apps/web'] }
  delete web.paths
  const stripped = { ...knip, workspaces: { ...knip.workspaces, 'apps/web': web } }
  const errors = validateWebTestGraphVisibility(repoRoot, stripped, webTestFile)
  assert.ok(
    errors.some((message) => message.includes('paths must map "@test/*"')),
    errors.join('\n')
  )
})

test('RED: an empty web test tree fails rather than passing vacuously', () => {
  const errors = validateWebTestGraphVisibility(repoRoot, knip, null)
  assert.deepEqual(errors, [
    'apps/web/test carries no *.test.ts(x) file — this guard would pass vacuously'
  ])
})

// ---------------------------------------------------------------------------
// Negative self-tests: hand-craft the exact bad state git's clean merge
// produces and prove the guard goes red, then (implicitly) restore by never
// touching the file on disk — these fixtures are in-memory clones only.
// ---------------------------------------------------------------------------

test('negative self-test: splicing two blocks (same key COUNT, swapped content) fails the path-ownership check', () => {
  // Simulate exactly what a clean-but-wrong merge produces: "apps/web" and
  // "packages/piing" keep their own keys, but their entry/project VALUES are
  // swapped — the shape a shared trailing brace splice leaves behind. Key
  // count and key set are both untouched, so only the path-ownership check
  // can catch this.
  const doctored = JSON.parse(JSON.stringify(knip))
  const webBlock = doctored.workspaces['apps/web']
  const piingBlock = doctored.workspaces['packages/piing']
  assert.ok(webBlock && piingBlock, 'fixture assumes both apps/web and packages/piing exist in the real config')
  doctored.workspaces['apps/web'] = piingBlock
  doctored.workspaces['packages/piing'] = webBlock

  const errors = validateKnipWorkspaceMap(repoRoot, doctored, workspacesGlobs)
  assert.ok(
    errors.some((m) => m.includes('"apps/web"') && m.includes('resolves to')),
    'expected apps/web to fail on piing\'s paths (e.g. src/extensionruntime/index.ts does not exist under apps/web)',
  )
  assert.ok(
    errors.some((m) => m.includes('"packages/piing"') && m.includes('resolves to')),
    'expected packages/piing to fail on web\'s paths (e.g. next.config.ts does not exist under packages/piing)',
  )
  // The key-set check must NOT fire for this fixture — proving the two
  // checks are independent and this splice shape needs the second one.
  const keyErrors = errors.filter((m) => m.includes('is missing key') || m.includes('is not a resolved workspace member'))
  assert.deepEqual(keyErrors, [], 'a same-key-count content swap must not trip the key-set check')
})

test('negative self-test: dropping a workspace key fails the key-set check', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  delete doctored.workspaces['packages/testing']

  const errors = validateKnipWorkspaceMap(repoRoot, doctored, workspacesGlobs)
  assert.ok(
    errors.some((m) => m.includes('missing key "packages/testing"')),
    'expected a missing-key violation for the dropped workspace',
  )
})

test('negative self-test: an extra/stale workspace key fails the key-set check', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  doctored.workspaces['packages/does-not-exist'] = { entry: [], project: [] }

  const errors = validateKnipWorkspaceMap(repoRoot, doctored, workspacesGlobs)
  assert.ok(
    errors.some((m) => m.includes('key "packages/does-not-exist" which is not a resolved workspace member')),
    'expected an extra-key violation for the stale workspace entry',
  )
})

test('negative self-test: removing the Chiefing public subpath entry fails the public-entry check', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  const chiefing = doctored.workspaces['packages/chiefing']
  assert.ok(chiefing && Array.isArray(chiefing.entry), 'fixture assumes Chiefing has a Knip entry list')
  chiefing.entry = chiefing.entry.filter((entry) => entry !== 'src/extensionruntime/index.ts')

  const errors = validateChiefingExtensionRuntimeKnipEntry(repoRoot, doctored)
  assert.ok(
    errors.some((m) => m.includes('src/extensionruntime/index.ts') && m.includes('explicit entry')),
    'expected a removed public source entry to fail the Knip-entry contract',
  )
})

test('negative self-test: removing either Piing extension graph edge fails the runtime-asset check', () => {
  for (const field of ['entry', 'project']) {
    const doctored = JSON.parse(JSON.stringify(knip))
    doctored.workspaces['packages/piing'][field] = doctored.workspaces['packages/piing'][
      field
    ].filter((pattern) => pattern !== 'extensions/*.ts')

    const errors = validatePiingRuntimeExtensionKnipGraph(repoRoot, doctored)
    assert.ok(
      errors.some((message) => message.includes('extensions/*.ts') && message.includes(field)),
      `expected missing Piing ${field} edge to fail, got: ${JSON.stringify(errors)}`
    )
  }
})

test('RED: omitting a private web SSE consumer entry fails the derived Knip contract', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  const web = doctored.workspaces['apps/web']
  const omitted = 'src/hooks/UsePersonStream.ts'
  web.entry = web.entry.filter((entry) => entry !== omitted)

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, doctored, webPackage)
  assert.ok(
    errors.some((message) => message.includes(omitted) && message.includes('literal entry')),
    `expected an omitted private web SSE root to fail, got: ${JSON.stringify(errors)}`
  )
})

test('RED: stale private web SSE metadata paths fail against the real source tree', () => {
  const doctoredWebPackage = JSON.parse(JSON.stringify(webPackage))
  doctoredWebPackage.chief.privateWebDeferredConsumerContracts.sse['S6-person'].entry =
    'src/hooks/RemovedPersonStream.ts'

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, knip, doctoredWebPackage)
  assert.ok(
    errors.some((message) => message.includes('S6-person') && message.includes('stale path')),
    `expected a stale metadata path to fail, got: ${JSON.stringify(errors)}`
  )
})

test('RED: stale private web SSE metadata exports fail against parsed real source', () => {
  const doctoredWebPackage = JSON.parse(JSON.stringify(webPackage))
  doctoredWebPackage.chief.privateWebDeferredConsumerContracts.sse['S5-lifecycle'].export =
    'retiredStreamLifecycle'

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, knip, doctoredWebPackage)
  assert.ok(
    errors.some((message) => message.includes('S5-lifecycle') && message.includes('stale export')),
    `expected a stale metadata export to fail, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a null private web SSE role and its removed matching root cannot shrink the contract', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  const doctoredWebPackage = JSON.parse(JSON.stringify(webPackage))
  doctoredWebPackage.chief.privateWebDeferredConsumerContracts.sse['S6-person'] = null
  const omitted = 'src/hooks/UsePersonStream.ts'
  doctored.workspaces['apps/web'].entry = doctored.workspaces['apps/web'].entry.filter(
    (entry) => entry !== omitted
  )

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, doctored, doctoredWebPackage)
  assert.ok(
    errors.some((message) => message.includes('S6-person') && message.includes('must be an object')),
    `expected a null required role to fail, got: ${JSON.stringify(errors)}`
  )
  assert.ok(
    errors.some((message) => message.includes('exactly 4 valid private-web SSE consumer contracts')),
    `expected a shrunk contract set to fail, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a broad Knip glob cannot substitute for a private web SSE literal root', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  const web = doctored.workspaces['apps/web']
  const omitted = 'src/services/SseClientService.ts'
  web.entry = web.entry.filter((entry) => entry !== omitted)
  web.entry.push('src/services/**/*.ts')

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, doctored, webPackage)
  assert.ok(
    errors.some((message) => message.includes('src/services/**/*.ts') && message.includes('broadly covers')),
    `expected a broad glob substitution to fail, got: ${JSON.stringify(errors)}`
  )
})

test('RED: a Knip ignore cannot substitute for a private web SSE literal root', () => {
  const doctored = JSON.parse(JSON.stringify(knip))
  const web = doctored.workspaces['apps/web']
  const omitted = 'src/services/SseClientService.ts'
  web.entry = web.entry.filter((entry) => entry !== omitted)
  web.ignore = ['src/services/**']

  const errors = validatePrivateWebSseConsumerKnipEntries(repoRoot, doctored, webPackage)
  assert.ok(
    errors.some((message) => message.includes('src/services/**') && message.includes('ignores deferred')),
    `expected an ignore substitution to fail, got: ${JSON.stringify(errors)}`
  )
})

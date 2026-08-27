// The repo's browser acceptance check must be RUNNABLE, and there must be
// exactly one of it.
//
// WHY THIS GUARD EXISTS. `scripts/browser-flow-check.mjs` was this repo's
// nominal browser acceptance check and had been doubly dead for some time:
//
//   1. it drove `apps/api` on :8791, and `apps/api` was DELETED;
//   2. it imported `playwright`, which is not a dependency of this repo, so
//      the file could not be loaded at all — never mind run.
//
// Nothing referenced it, nothing ran it, and every handoff kept quoting its
// last green number ("19/19", "22/22") as if it still meant something. That is
// the worst shape a check can take: it occupies the slot, so nobody writes the
// one that works.
//
// Neither defect is detectable by any other gate here. `typecheck` does not
// look at `scripts/*.mjs`, `knip` does not resolve a dead app path, and no
// suite imports the file. So both are checked directly, and they are checked
// by DERIVING the browser check from the tree rather than naming it: a second
// browser check appearing is itself the failure, because two drivers is two
// sources of truth about how to reach a browser.
//
// Run with `node --test scripts/test/browser-check-runnable.test.mjs`.
import assert from 'node:assert/strict'
import { readdirSync, readFileSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = join(here, '..', '..')
const scriptsDir = join(repoRoot, 'scripts')

/** Every browser acceptance check on disk, by convention `browser-*-check.mjs`.
 *
 * Derived, never named: a maintained name is how the dead one kept its slot. */
function browserChecks() {
  return readdirSync(scriptsDir)
    .filter((name) => /^browser-.*-check\.mjs$/.test(name))
    .sort()
}

/** Bare module specifiers a source file imports — `node:` builtins and
 * relative paths excluded, because neither can be an undeclared dependency. */
function bareImports(source) {
  const found = new Set()
  for (const match of source.matchAll(/(?:^|\n)\s*import\s[^\n]*?from\s+['"]([^'"]+)['"]/g)) {
    found.add(match[1])
  }
  for (const match of source.matchAll(/\bimport\(\s*['"]([^'"]+)['"]\s*\)/g)) {
    found.add(match[1])
  }
  return [...found].filter(
    (specifier) => !specifier.startsWith('node:') && !specifier.startsWith('.')
  )
}

/** Every dependency this repo declares anywhere — root and every workspace
 * member. Root-hoisted devDependencies count, the same convention
 * `dep-declaration.test.mjs` already established. */
function declaredPackages() {
  const declared = new Set()
  const manifests = [join(repoRoot, 'package.json')]
  for (const area of ['apps', 'packages']) {
    const root = join(repoRoot, area)
    if (!existsSync(root)) continue
    for (const member of readdirSync(root)) {
      const manifest = join(root, member, 'package.json')
      if (existsSync(manifest)) manifests.push(manifest)
    }
  }
  for (const path of manifests) {
    const json = JSON.parse(readFileSync(path, 'utf8'))
    for (const field of ['dependencies', 'devDependencies']) {
      for (const name of Object.keys(json[field] ?? {})) declared.add(name)
    }
  }
  return declared
}

test('the repo has exactly one browser acceptance check', () => {
  const checks = browserChecks()
  // ZERO is a failure, not a vacuous pass: a browser proof is the only thing
  // that can see the class of defect that reaches an operator's screen, and
  // this program has shipped four of them found no other way.
  assert.deepEqual(
    checks,
    ['browser-org-tools-check.mjs'],
    `expected exactly one browser acceptance check, found: ${checks.join(', ') || '(none)'}`
  )
})

test('the browser check can actually be imported', async () => {
  // The load-bearing assertion. `playwright` was imported at module scope, so
  // the previous check threw ERR_MODULE_NOT_FOUND before its first line ran —
  // for months, invisibly, because nothing ever imported it. This does.
  for (const name of browserChecks()) {
    await import(join(scriptsDir, name))
  }
})

test('the browser check imports nothing this repo does not declare', () => {
  const declared = declaredPackages()
  for (const name of browserChecks()) {
    const source = readFileSync(join(scriptsDir, name), 'utf8')
    const undeclared = bareImports(source).filter((specifier) => {
      // A subpath import (`pkg/thing`) is declared by its package name.
      const packageName = specifier.startsWith('@')
        ? specifier.split('/').slice(0, 2).join('/')
        : specifier.split('/')[0]
      return !declared.has(packageName)
    })
    assert.deepEqual(
      undeclared,
      [],
      `${name} imports package(s) this repo does not declare: ${undeclared.join(', ')}`
    )
  }
})

test('the browser check names no workspace member that has been deleted', () => {
  // The other half of the previous check's death: it drove `apps/api`, which
  // no longer exists. A dead path in a driver reads exactly like a live one.
  for (const name of browserChecks()) {
    const source = readFileSync(join(scriptsDir, name), 'utf8')
    const missing = [...source.matchAll(/\b(apps|packages)\/([a-z0-9][a-z0-9-]*)/g)]
      .map((match) => `${match[1]}/${match[2]}`)
      .filter((path, index, all) => all.indexOf(path) === index)
      .filter((path) => !existsSync(join(repoRoot, path)))
    assert.deepEqual(
      missing,
      [],
      `${name} names workspace path(s) that do not exist: ${missing.join(', ')}`
    )
  }
})

test('the browser check does not run its flow on import', () => {
  // The import test above is only safe — and only honest — if loading the file
  // does not start a browser. The guard is the `import.meta.url` main check
  // every runnable script in this repo uses.
  for (const name of browserChecks()) {
    const source = readFileSync(join(scriptsDir, name), 'utf8')
    assert.ok(
      source.includes('import.meta.url === `file://${process.argv[1]}`'),
      `${name} must guard its entry point so importing it does not run the flow`
    )
  }
})

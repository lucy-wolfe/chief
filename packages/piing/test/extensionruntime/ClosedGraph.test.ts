import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

import ts from 'typescript'
import { describe, expect, it } from 'vitest'

const RUNTIME_ROOT = fileURLToPath(new URL('../../src/extensionruntime', import.meta.url))
const PIING_SOURCE_ROOT = fileURLToPath(new URL('../../src', import.meta.url))

interface ImportOccurrence {
  file: string
  specifier: string
  typeOnly: boolean
}

function walk(dir: string): string[] {
  const files: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) files.push(...walk(path))
    else if (entry.isFile() && entry.name.endsWith('.ts')) files.push(path)
  }
  return files
}

function imports(file: string): readonly ImportOccurrence[] {
  const source = ts.createSourceFile(file, readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true)
  const occurrences: ImportOccurrence[] = []
  const visit = (node: ts.Node): void => {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      const typeOnly = ts.isImportDeclaration(node)
        ? node.importClause?.isTypeOnly === true
        : node.isTypeOnly
      occurrences.push({ file, specifier: node.moduleSpecifier.text, typeOnly })
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
  return occurrences
}

function allowed(occurrence: ImportOccurrence): boolean {
  if (occurrence.specifier.startsWith('./') || occurrence.specifier.startsWith('../')) return true
  if (occurrence.specifier.startsWith('node:')) return true
  return occurrence.typeOnly && occurrence.specifier.startsWith('@earendil-works/')
}

describe('@chief/piing/extension-runtime closed graph', () => {
  const files = walk(RUNTIME_ROOT).sort()
  const occurrences = files.flatMap(imports)

  it('has a non-vacuous source graph', () => {
    expect(files.map((file) => relative(RUNTIME_ROOT, file))).toEqual([
      'ChiefLogo.ts',
      'LaunchTrace.ts',
      'OrganizationTools.ts',
      'ReloadSentinel.ts',
      'RuntimePolicy.ts',
      'index.ts'
    ])
  })

  it('contains only relative siblings, node builtins, and type-only Pi imports', () => {
    const forbidden = occurrences.filter((occurrence) => !allowed(occurrence))
    expect(forbidden).toEqual([])
  })

  it('rejects a runtime dependency that could not survive copy materialization', () => {
    expect(
      allowed({
        file: join(dirname(RUNTIME_ROOT), 'negative.ts'),
        specifier: '@chief/chiefing',
        typeOnly: false
      })
    ).toBe(false)
    expect(
      allowed({
        file: join(dirname(RUNTIME_ROOT), 'negative.ts'),
        specifier: '@earendil-works/pi-coding-agent',
        typeOnly: false
      })
    ).toBe(false)
  })

  it('keeps Chiefing a caller-supplied source entry rather than a Piing runtime import', () => {
    const chiefingImports = walk(PIING_SOURCE_ROOT)
      .flatMap(imports)
      .filter((occurrence) => occurrence.specifier.startsWith('@chief/chiefing'))

    expect(chiefingImports).toEqual([])
  })
})

import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { basename, dirname, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import ts from 'typescript'

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const sourceRoot = resolve(packageRoot, 'src')

export const extensionRuntimeEntry = resolve(sourceRoot, 'extensionruntime', 'index.ts')

function sourceFile(path: string): ts.SourceFile {
  return ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true)
}

function moduleSpecifierNodes(file: ts.SourceFile): ts.StringLiteral[] {
  const nodes: ts.StringLiteral[] = []
  const visit = (node: ts.Node): void => {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      nodes.push(node.moduleSpecifier)
    }
    ts.forEachChild(node, visit)
  }
  visit(file)
  return nodes
}

function resolveRelativeModule(importer: string, specifier: string): string {
  const direct = resolve(dirname(importer), specifier)
  const candidates = direct.endsWith('.js')
    ? [direct, `${direct.slice(0, -'.js'.length)}.ts`]
    : [direct, `${direct}.ts`, resolve(direct, 'index.ts')]
  const resolved = candidates.find((candidate) => existsSync(candidate))
  if (!resolved) {
    throw new Error(`extension-runtime import ${specifier} from ${importer} does not resolve`)
  }
  return resolved
}

export function staticModuleSpecifiers(path: string): string[] {
  return moduleSpecifierNodes(sourceFile(path)).map((node) => node.text)
}

/** Walk every type and value edge that TypeScript needs when Pi compiles the
 * flattened copy. The graph is intentionally discovered, not maintained as a
 * second stale inventory beside the real imports. */
export function walkExtensionRuntimeGraph(): string[] {
  const seen = new Set<string>()
  const visit = (path: string): void => {
    if (seen.has(path)) return
    seen.add(path)
    for (const specifier of staticModuleSpecifiers(path)) {
      if (specifier.startsWith('.')) visit(resolveRelativeModule(path, specifier))
    }
  }
  visit(extensionRuntimeEntry)
  return [...seen].sort()
}

export function sourceRelativePath(path: string): string {
  return relative(sourceRoot, path).replaceAll('\\', '/')
}

function flatCopyName(path: string): string {
  return `chiefing-runtime-${basename(path)}`
}

function rewriteRelativeSpecifiers(path: string, closure: readonly string[]): string {
  const content = readFileSync(path, 'utf8')
  const parsed = ts.createSourceFile(path, content, ts.ScriptTarget.Latest, true)
  const copyNames = new Map(closure.map((sourcePath) => [sourcePath, flatCopyName(sourcePath)]))
  const replacements: Array<{ start: number; end: number; value: string }> = []

  for (const node of moduleSpecifierNodes(parsed)) {
    if (!node.text.startsWith('.')) continue
    const target = resolveRelativeModule(path, node.text)
    const copyName = copyNames.get(target)
    if (!copyName) {
      throw new Error(`extension-runtime copy lacks ${target}, imported by ${path}`)
    }
    replacements.push({
      start: node.getStart(parsed) + 1,
      end: node.getEnd() - 1,
      value: `./${copyName}`
    })
  }

  let rewritten = content
  for (const replacement of replacements.sort((a, b) => b.start - a.start)) {
    rewritten =
      rewritten.slice(0, replacement.start) + replacement.value + rewritten.slice(replacement.end)
  }
  return rewritten
}

/** Materialize the same flat naming E3-S6 consumes, with deliberately no
 * tsconfig paths or package dependencies. `node:crypto` and unref-able Node
 * timers are the only Node surfaces the closure uses, so the minimal
 * declaration below models those runtime builtins rather than smuggling in
 * `@types/node` through a parent node_modules. */
export function materializeExtensionRuntimeCopy(destination: string): {
  closure: string[]
  copiedEntry: string
} {
  const closure = walkExtensionRuntimeGraph()
  const copyNames = closure.map(flatCopyName)
  if (new Set(copyNames).size !== copyNames.length) {
    throw new Error('extension-runtime flat copy has colliding basenames')
  }
  for (const path of closure) {
    writeFileSync(
      resolve(destination, flatCopyName(path)),
      rewriteRelativeSpecifiers(path, closure),
      'utf8'
    )
  }
  writeFileSync(resolve(destination, 'package.json'), '{"type":"module"}\n', 'utf8')
  writeFileSync(
    resolve(destination, 'node-builtins.d.ts'),
    // The Node surfaces the closure actually uses, modelled by hand rather
    // than by pulling @types/node in through a parent node_modules — which is
    // precisely the thing this materialization proves the copy does without.
    // The `node:crypto` key/sign/verify half, `node:fs`, `node:path` and
    // `Buffer` arrived with #751/P7, when a pane became responsible for reading
    // and redeeming its own identity key. `ClosedGraph.test.ts` pins the fs
    // reach to that one module, so this fixture cannot quietly become a
    // licence for the rest of the graph to touch the disk.
    "declare module 'node:crypto' {\n" +
      '  interface Hash {\n' +
      '    update(value: string): Hash\n' +
      "    digest(encoding: 'hex'): string\n" +
      '  }\n' +
      '  export function createHash(algorithm: string): Hash\n' +
      '  interface KeyObject {\n' +
      '    export(options: { type: string; format: string }): Buffer\n' +
      '  }\n' +
      '  export function createPrivateKey(key: string | object): KeyObject\n' +
      '  export function createPublicKey(key: string | object): KeyObject\n' +
      '  export function generateKeyPairSync(\n' +
      '    type: string,\n' +
      '    options: object\n' +
      '  ): { privateKey: string; publicKey: Buffer }\n' +
      '  export function sign(\n' +
      '    algorithm: string,\n' +
      '    data: Buffer,\n' +
      '    key: KeyObject | object\n' +
      '  ): Buffer\n' +
      '  export function verify(\n' +
      '    algorithm: string,\n' +
      '    data: Buffer,\n' +
      '    key: KeyObject | object,\n' +
      '    signature: Buffer\n' +
      '  ): boolean\n' +
      '}\n' +
      '\n' +
      "declare module 'node:fs' {\n" +
      '  export function chmodSync(path: string, mode: number): void\n' +
      '  export function copyFileSync(source: string, destination: string): void\n' +
      '  export function existsSync(path: string): boolean\n' +
      '  export function mkdirSync(path: string, options?: { recursive?: boolean }): void\n' +
      "  export function readFileSync(path: string, encoding: 'utf8'): string\n" +
      // `statSync().mode` is the pane's half of the identity-key permission
      // rule: a key readable by anyone but its owner is not used. Only the one
      // member the closure actually calls is declared, for the same reason
      // every other member here is — this stub exists to prove the flat copy
      // needs nothing the pi-home cannot give it.
      '  export function statSync(path: string): { mode: number }\n' +
      '  export function writeFileSync(\n' +
      '    path: string,\n' +
      '    data: string,\n' +
      '    options?: { mode?: number }\n' +
      '  ): void\n' +
      '}\n' +
      '\n' +
      "declare module 'node:path' {\n" +
      '  export function dirname(path: string): string\n' +
      '  export function join(...segments: string[]): string\n' +
      '}\n' +
      '\n' +
      'declare class Buffer extends Uint8Array {\n' +
      '  static from(value: string, encoding?: string): Buffer\n' +
      '  static concat(list: Buffer[]): Buffer\n' +
      '  toString(encoding?: string): string\n' +
      '}\n' +
      '\n' +
      'declare namespace NodeJS {\n' +
      '  interface Timeout {\n' +
      '    unref(): Timeout\n' +
      '  }\n' +
      '}\n' +
      '\n' +
      'declare function setTimeout(\n' +
      '  handler: Function,\n' +
      '  timeout?: number,\n' +
      '  ...arguments: unknown[]\n' +
      '): NodeJS.Timeout\n' +
      'declare function clearTimeout(timeout?: NodeJS.Timeout | number): void\n',
    'utf8'
  )
  writeFileSync(
    resolve(destination, 'tsconfig.json'),
    '{\n' +
      '  "compilerOptions": {\n' +
      '    "target": "ESNext",\n' +
      '    "module": "NodeNext",\n' +
      '    "moduleResolution": "NodeNext",\n' +
      '    "allowImportingTsExtensions": true,\n' +
      '    "lib": ["ESNext", "DOM"],\n' +
      '    "noEmit": true,\n' +
      '    "skipLibCheck": true,\n' +
      '    "strict": true\n' +
      '  },\n' +
      '  "include": ["*.ts", "node-builtins.d.ts"]\n' +
      '}\n',
    'utf8'
  )
  return {
    closure,
    copiedEntry: resolve(destination, flatCopyName(extensionRuntimeEntry))
  }
}

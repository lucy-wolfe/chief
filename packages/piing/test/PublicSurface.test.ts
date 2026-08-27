import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import * as ExtensionRuntime from '@/extensionruntime/index'
import * as Piing from '@/index'

const NOT_IMPLEMENTED_PATTERN = /^not implemented: @chief\/piing stub — implemented by E3-S\d/

const SRC_ROOT = fileURLToPath(new URL('../src', import.meta.url))

function walkTsFiles(dir: string): string[] {
  const out: string[] = []
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      out.push(...walkTsFiles(path))
    } else if (entry.isFile() && entry.name.endsWith('.ts')) {
      out.push(path)
    }
  }
  return out
}

describe('barrel exports every contracted value symbol', () => {
  it('runtime symbols exist with the right typeof', () => {
    expect(typeof Piing.piingPackageRoot).toBe('function')
    expect(typeof Piing.workspaceNodeModulesRoot).toBe('function')
    expect(typeof Piing.piingExtensionsRoot).toBe('function')
    expect(typeof Piing.piingSkillsRoot).toBe('function')
    // TOMBSTONE: `resolvePiBinary`, `piPackageRoot`, `piRpcEntryPath`,
    // `piRpcClientEntryPath` (the whole `PiBinary` module, already caller-less
    // since the resolver was ported to Rust) and `pinnedPiArtifacts` /
    // `attestPiRuntime` (which attested the PATCHED build and has no subject
    // now the patch is deleted).
  })

  it('policy symbols exist with the right typeof', () => {
    expect(Array.isArray(Piing.BUILTIN_TOOLS)).toBe(true)
  })

  it('home symbols exist with the right typeof', () => {
    expect(typeof Piing.organizationPersonAccent).toBe('function')
    expect(typeof Piing.identityAccentOrder).toBe('function')
  })
})

describe('@chief/piing/extension-runtime public surface', () => {
  it('exports the promoted organization contracts from the dedicated subpath', () => {
    expect(Array.isArray(ExtensionRuntime.ORGANIZATION_ACTIVE_RUNTIME_TOOL_NAMES)).toBe(true)
    expect(Array.isArray(ExtensionRuntime.ORGANIZATION_MANAGER_TOOL_NAMES)).toBe(true)
    expect(Array.isArray(ExtensionRuntime.ORGANIZATION_ROOT_EXECUTIVE_TOOL_NAMES)).toBe(true)
    expect(typeof ExtensionRuntime.organizationForegroundResponsivenessContract).toBe('function')
  })
})

/* TOMBSTONE: the E3-S1 stub-contract case over `resolvePiBinary`. The symbol is
 * deleted with the whole `PiBinary` module, so there is no export left to prove
 * implemented — the same disposition as the two E3-S3 cases below. */

/* TOMBSTONE (chief-home-is-cwd §3/§4e): the two E3-S3 stub-contract cases,
 * over `isForbiddenLauncherResource` and `buildCatalog`. Both symbols are
 * deleted with per-person resource selection, so there is no export left to
 * prove implemented. */

describe('E3-S3 IdentityTheme is implemented and no longer throws the stub contract', () => {
  it('organizationPersonAccent resolves instead of throwing not-implemented', () => {
    expect(() => Piing.organizationPersonAccent(['alice'], 'alice')).not.toThrow(
      NOT_IMPLEMENTED_PATTERN
    )
  })
})

describe('real constants match the contract exactly', () => {
  /* TOMBSTONE: `PINNED_PI_VERSION`. It pinned the version the ATTESTATION
   * hashed, and both are deleted. The dependency version itself is still
   * pinned — by `packages/piing/package.json` and the lockfile, which is where
   * a version pin belongs. */

  it('BUILTIN_TOOLS', () => {
    expect(Piing.BUILTIN_TOOLS).toEqual(['read', 'bash', 'edit', 'write', 'grep', 'find', 'ls'])
  })
})

// Strips `/** ... */`, `/* ... */`, and `// ...` comments so the process.env
// scan below checks actual code, not the many provenance/rationale comments
// in this package that legitimately SAY "process.env" while explaining that
// the module never reads it.
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
}

describe('src/** never reads process.env and never imports the legacy tree', () => {
  const files = walkTsFiles(SRC_ROOT)

  it('scanned at least one file (sanity check the scan itself is not vacuous)', () => {
    expect(files.length).toBeGreaterThan(10)
  })

  it('no file references process.env outside a comment', () => {
    const offenders = files.filter((file) =>
      stripComments(readFileSync(file, 'utf8')).includes('process.env')
    )
    expect(offenders).toEqual([])
  })

  it('negative self-check: the scan actually flags real process.env usage (not vacuously green)', () => {
    const offenders = ['const x = process.env.FOO'].filter((line) =>
      stripComments(line).includes('process.env')
    )
    expect(offenders).toHaveLength(1)
  })

  it('no file imports the legacy tree (src/ or extensions/ outside this package)', () => {
    const legacyImportPattern = /from\s+['"](\.\.\/)+(src|extensions)\//
    const offenders = files.filter((file) => legacyImportPattern.test(readFileSync(file, 'utf8')))
    expect(offenders).toEqual([])
  })
})

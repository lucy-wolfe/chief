import { createHash } from 'node:crypto'
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'

import { afterEach, describe, expect, test } from 'vitest'

import {
  assetName,
  packageRelease,
  parseArgs,
  RELEASE_TARGETS,
  targetArtifactDir
} from '../../../scripts/package-release'
import {
  assembleVersionTree,
  EXTENSION_RUNTIME_SHIMS,
  hostTarget,
  piFloor,
  releaseVersion,
  RESOURCE_SUBTREES
} from '../../../scripts/release-chiefd'

// The release ARTIFACT is what `chief upgrade` and `install.sh` unpack, and it
// is produced by two paths that must agree byte-for-byte on shape and manifest:
// `bun run release` (into ~/.chief) and `scripts/package-release.ts` (a CI
// tarball). Both go through `assembleVersionTree`, so this suite pins that one
// emitter plus the packager's own target-awareness — the one thing the packager
// adds over the installer, and the one thing that, wrong, ships a runner's
// architecture under a cross target's name.
//
// The manifest is verified as TEXT, on purpose: a `JSON.parse` would need a
// type assertion this package's lint bans, and the thing under test is exactly
// the bytes the manifest carries.

const LINUX = 'x86_64-unknown-linux-gnu'
const DARWIN = 'aarch64-apple-darwin'
const BINARIES = ['chief', 'chiefd', 'beacond']

const scratches: string[] = []
function scratch(): string {
  const dir = mkdtempSync(join(tmpdir(), 'release-artifact-'))
  scratches.push(dir)
  return dir
}
afterEach(() => {
  for (const dir of scratches.splice(0)) rmSync(dir, { recursive: true, force: true })
})

/**
 * A fixture checkout carrying exactly what a release READS: three built
 * binaries at the given target's artifact dir, one file in each resource
 * subtree, the Pi-floor constant, and a workspace version.
 */
function seedCheckout(target: string): string {
  const root = scratch()
  const artifacts = targetArtifactDir(root, target)
  mkdirSync(artifacts, { recursive: true })
  for (const name of BINARIES) {
    writeFileSync(join(artifacts, name), `BINARY:${name}`)
    chmodSync(join(artifacts, name), 0o755)
  }
  for (const subtree of RESOURCE_SUBTREES) {
    mkdirSync(join(root, subtree), { recursive: true })
    writeFileSync(join(root, subtree, 'payload.txt'), `payload of ${subtree}`)
  }
  // The runtime each shim points at. The release REFUSES to write a shim whose
  // target it did not package -- deliberately, so a subtree deleted from
  // RESOURCE_SUBTREES fails the release rather than shipping a dangling
  // pointer -- so this fixture must produce the file, not only the directory.
  for (const { from } of EXTENSION_RUNTIME_SHIMS) {
    mkdirSync(dirname(join(root, from)), { recursive: true })
    writeFileSync(join(root, from), `runtime of ${from}`)
  }
  const piFloorDir = join(root, 'apps', 'chiefd', 'crates', 'host-primitives', 'src')
  mkdirSync(piFloorDir, { recursive: true })
  const piFloorLine = 'pub const MINIMUM_PI_VERSION: &str = "0.80.10";\n'
  writeFileSync(join(piFloorDir, 'pi_floor.rs'), piFloorLine)
  mkdirSync(join(root, 'apps', 'chiefd'), { recursive: true })
  writeFileSync(
    join(root, 'apps', 'chiefd', 'Cargo.toml'),
    '[workspace.package]\nversion = "9.9.9"\n'
  )
  return root
}

function binaryPaths(root: string, target: string): Record<string, string> {
  const dir = targetArtifactDir(root, target)
  return Object.fromEntries(BINARIES.map((name) => [name, join(dir, name)]))
}

function sha256(path: string): string {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function readManifest(dir: string): string {
  return readFileSync(join(dir, 'manifest.json'), 'utf8')
}

describe('assembleVersionTree', () => {
  test('writes bin, resources and a manifest naming the requested target and Pi floor', () => {
    const root = seedCheckout(LINUX)
    const tree = scratch()

    assembleVersionTree(tree, '2.0.7', binaryPaths(root, LINUX), root, DARWIN)

    const manifest = readManifest(tree)
    expect(manifest).toContain('"version": "2.0.7"')
    // The target the caller PASSES, never the host the packager runs on — a
    // cross build's manifest must name what it built, not what built it.
    expect(manifest).toContain(`"target": "${DARWIN}"`)
    expect(manifest).toContain('"piFloor": "0.80.10"')
    // Checksums are of the staged bytes.
    expect(manifest).toContain(sha256(join(tree, 'bin', 'chief')))
    for (const subtree of RESOURCE_SUBTREES) {
      expect(manifest).toContain(sha256(join(tree, 'resources', `${subtree}/payload.txt`)))
    }
  })

  test('refuses a zero-byte binary rather than shipping it', () => {
    const root = seedCheckout(LINUX)
    const empty = join(scratch(), 'chief')
    writeFileSync(empty, '')
    chmodSync(empty, 0o755)
    const binaries = { ...binaryPaths(root, LINUX), chief: empty }
    expect(() => assembleVersionTree(scratch(), '2.0.7', binaries, root, LINUX)).toThrow(
      /zero-byte/
    )
  })

  test('refuses a missing resource subtree with an actionable message', () => {
    const root = seedCheckout(LINUX)
    rmSync(join(root, RESOURCE_SUBTREES[0]), { recursive: true, force: true })
    const bins = binaryPaths(root, LINUX)
    expect(() => assembleVersionTree(scratch(), '2.0.7', bins, root, LINUX)).toThrow(
      new RegExp(RESOURCE_SUBTREES[0])
    )
  })
})

describe('packageRelease', () => {
  test('assembles the tree and hands the archiver bin, resources and manifest', () => {
    const root = seedCheckout(DARWIN)
    let archivedFrom: string | undefined
    let archivedTo: string | undefined

    const result = packageRelease({
      target: DARWIN,
      root,
      outDir: join(root, 'dist'),
      skipCargo: true,
      archive: (tarball, treeDir) => {
        archivedTo = tarball
        archivedFrom = treeDir
        // The tree the archiver receives is complete at call time.
        expect(readManifest(treeDir)).toContain(`"target": "${DARWIN}"`)
        expect(readFileSync(join(treeDir, 'bin', 'chief'), 'utf8')).toBe('BINARY:chief')
      }
    })

    expect(result.version).toBe('9.9.9')
    expect(result.tarball).toBe(join(root, 'dist', `chief-9.9.9-${DARWIN}.tar.gz`))
    expect(archivedTo).toBe(result.tarball)
    expect(archivedFrom).toBeTypeOf('string')
  })

  test('refuses a target it does not publish', () => {
    expect(() => packageRelease({ target: 'x86_64-pc-windows-msvc', skipCargo: true })).toThrow(
      /macOS and Linux only/
    )
  })
})

describe('the packager helpers', () => {
  test('the asset name is the one chief upgrade downloads', () => {
    expect(assetName('2.0.7', 'x86_64-apple-darwin')).toBe('chief-2.0.7-x86_64-apple-darwin.tar.gz')
  })

  test('a --target build resolves under target/<triple>/release, never bare target/release', () => {
    expect(targetArtifactDir('/repo', DARWIN)).toBe(`/repo/apps/chiefd/target/${DARWIN}/release`)
  })

  test('parseArgs requires --target and reads --out', () => {
    expect(parseArgs(['--target', LINUX])).toEqual({ target: LINUX, outDir: undefined })
    expect(parseArgs(['--target', DARWIN, '--out', '/tmp/out'])).toEqual({
      target: DARWIN,
      outDir: '/tmp/out'
    })
    expect(() => parseArgs([])).toThrow(/--target/)
    expect(() => parseArgs(['--nope'])).toThrow(/unknown argument/)
  })

  test('every published target is a real cross triple', () => {
    expect(RELEASE_TARGETS).toContain(hostTarget('linux', 'x64'))
    expect(RELEASE_TARGETS).toContain(hostTarget('darwin', 'arm64'))
  })
})

describe('releaseVersion', () => {
  test('the stamped env version wins over the workspace version', () => {
    const root = seedCheckout(LINUX)
    expect(releaseVersion(root, { CHIEF_RELEASE_VERSION: '2.0.42' })).toBe('2.0.42')
    expect(releaseVersion(root, {})).toBe('9.9.9')
  })

  test('a version that would become a path is refused', () => {
    const root = seedCheckout(LINUX)
    expect(() => releaseVersion(root, { CHIEF_RELEASE_VERSION: '../evil' })).toThrow()
  })

  test('the Pi floor is parsed from its one Rust definition', () => {
    const root = seedCheckout(LINUX)
    expect(piFloor(root)).toBe('0.80.10')
  })
})

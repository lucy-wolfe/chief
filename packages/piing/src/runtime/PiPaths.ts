/**
 * Absolute-path resolvers for the piing package's own install location and
 * its asset roots. Implemented by E3-S1 (`piing-scaffold-runtime-pinning`);
 * E3-S7 makes the extension and skill asset roots real package directories.
 */
import { realpathSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** Absolute root of the installed @chief/piing package (realpath-resolved).
 * Derived from this module's own location: `src/runtime/PiPaths.ts` and its
 * built `dist/runtime/PiPaths.js` are equally two directories below the
 * package root, so walking up two levels works from either. */
export function piingPackageRoot(): string {
  const moduleDir = dirname(fileURLToPath(import.meta.url))
  return realpathSync(resolve(moduleDir, '..', '..'))
}

/** The workspace root's node_modules (hoisted install target):
 * `piingPackageRoot()/../../node_modules`, overridable for tests. All other
 * resolvers root here by default. */
export function workspaceNodeModulesRoot(override?: string): string {
  if (override) return override
  return resolve(piingPackageRoot(), '..', '..', 'node_modules')
}

/** `packages/piing/extensions` — extension deployment assets. */
export function piingExtensionsRoot(): string {
  return join(piingPackageRoot(), 'extensions')
}

/** `packages/piing/skills` — Pi skill assets. */
export function piingSkillsRoot(): string {
  return join(piingPackageRoot(), 'skills')
}

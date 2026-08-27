/**
 * Types describing what piing projects OUT of the host environment: the tool
 * floor.
 *
 * TOMBSTONE (chief-home-is-cwd §3/§4e): `ResourceItem`, `ResourceCatalog`,
 * `PackageItem` and `CatalogRoots` described the catalog assembled from a Pi
 * home's installed skills, extensions and packages. Pi discovers and validates
 * skills itself, so nothing here assembles one.
 *
 * Housed here (rather than beside their value modules) per
 * `lucy/no-exported-type-outside-types-dir` — every exported type/interface
 * in this package lives under `src/types/` (E0-S5/#756).
 */

// --- policy/CapabilityPolicy.ts --------------------------------------------
// copied from src/foundation/capability-policy.ts:7 (E0-S5); E3-S3 takes ownership.
export type BuiltinTool = 'read' | 'bash' | 'edit' | 'write' | 'grep' | 'find' | 'ls'

/**
 * Launcher-owned capability policy: the built-in tool floor.
 *
 * TOMBSTONE (chief-home-is-cwd §3/§4e): `isForbiddenLauncherResource` stood
 * beside it and kept chief's own implementation skills and extensions out of a
 * materialized person. Nothing is materialized and no resource is selected —
 * Pi loads the company's `.pi/skills` through one symlink — so the guard had
 * nothing left to exclude from anything. Moved from
 * `src/foundation/capability-policy.ts:7-19`. Implemented by E3-S3
 * (`pi-policy-modules`).
 */
import type { BuiltinTool } from '@/types/ProjectionTypes'

// real — copied from src/foundation/capability-policy.ts:7-16 (E0-S5).
export const BUILTIN_TOOLS: readonly BuiltinTool[] = [
  'read',
  'bash',
  'edit',
  'write',
  'grep',
  'find',
  'ls'
] as const

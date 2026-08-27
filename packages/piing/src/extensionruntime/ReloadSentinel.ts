/**
 * The single copy of the reload-adoption contract, replacing the
 * `RELOAD_ADOPTION_SENTINEL_BASENAME` duplication between the legacy runtime
 * drift detector and the Pi-side reader of the same file.
 * Behind the `./extension-runtime` subpath (see GoalPriority.ts's header for
 * the self-contained-closure constraint).
 */
import { join } from 'node:path'

// real — verified against src/organization/org-extension-runtime-drift.ts:19 (E0-S5).
export const RELOAD_ADOPTION_SENTINEL_BASENAME = '.extension-reload-adopted'

/** Absolute path to a person's reload-adoption sentinel, given their pi-home. */
export function reloadAdoptionSentinelPath(piHome: string): string {
  return join(piHome, RELOAD_ADOPTION_SENTINEL_BASENAME)
}

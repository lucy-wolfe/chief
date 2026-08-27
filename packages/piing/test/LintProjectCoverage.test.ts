import { readdirSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { join, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

import { ESLint } from 'eslint'
import { describe, expect, it } from 'vitest'

const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url))
const MOVED_ASSET_ROOTS = [
  {
    root: join(PACKAGE_ROOT, 'extensions'),
    project: '../../tsconfig.extensions.json',
    expectedSources: [
      'extensions/attached-input-observability.ts',
      'extensions/bus-events-bounded-append.ts',
      'extensions/card-style.ts',
      'extensions/chief-logo.ts',
      'extensions/company-stop.ts',
      'extensions/founder-launch.ts',
      'extensions/org-send-replay.ts',
      'extensions/organization-activity-status.ts',
      'extensions/organization-intercom.ts',
      'extensions/organization-runtime-policy.ts',
      'extensions/team-ui.ts',
      'extensions/tribes-welcome.ts'
    ]
  }
]

/**
 * #1041: a budget DERIVED from the contention this test actually runs under,
 * rather than a constant that silently assumes an idle box.
 *
 * The old value was a bare `30_000`. Run alone this test takes 7-15s; run as
 * part of the full parallel suite, beside an 81-second sibling, it exceeded
 * 30s and failed. Two separate agents diagnosed that as contention rather
 * than a regression, and each spent time proving it. A test whose pass
 * depends on what else is running is a flake, and a flake in the standing
 * gate list teaches people to re-run reds instead of reading them.
 *
 * Why THIS number is right rather than a bigger arbitrary one. A timeout
 * exists to stop an unbounded wait, and this test has no hang mode: it awaits
 * ESLint, which either finishes or throws. So the budget's only job is to be
 * a ceiling, and its only wrong values are "below what the work legitimately
 * costs". The work is CPU-bound — a full type-aware TypeScript program
 * over the extensions tree — and vitest schedules up to
 * `availableParallelism()` worker processes across the same cores. Fully
 * contended, identical work therefore
 * takes up to that multiple of its serial time; that factor is not slack, it
 * is the definition of sharing N cores N ways. `SERIAL_BUDGET_MS` is the
 * observed serial worst case, and the product is the honest ceiling. On a
 * single-core box it collapses back to the serial cost, which is correct.
 */
const SERIAL_BUDGET_MS = 15_000
const LINT_COVERAGE_BUDGET_MS = SERIAL_BUDGET_MS * Math.max(1, availableParallelism())

function walkTsFiles(root: string): string[] {
  const sources: string[] = []
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name)
    if (entry.isDirectory()) sources.push(...walkTsFiles(path))
    else if (entry.isFile() && entry.name.endsWith('.ts')) sources.push(path)
  }
  return sources
}

function packageRelativePath(path: string): string {
  return relative(PACKAGE_ROOT, path).split(sep).join('/')
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Object.prototype.toString.call(value) === '[object Object]'
}

function configuredProject(config: unknown): unknown {
  if (!isRecord(config)) return
  const languageOptions = config.languageOptions
  if (!isRecord(languageOptions)) return
  const parserOptions = languageOptions.parserOptions
  if (!isRecord(parserOptions)) return
  return parserOptions.project
}

describe('Piing lint project coverage', () => {
  it(
    'type-aware linting parses every checked-in moved extension source',
    async () => {
      const assetRoots = MOVED_ASSET_ROOTS.map((assetRoot) => ({
        ...assetRoot,
        sources: walkTsFiles(assetRoot.root).sort()
      }))
      const movedAssetSources = assetRoots.flatMap((assetRoot) => assetRoot.sources)

      for (const assetRoot of assetRoots) {
        expect(assetRoot.sources.map(packageRelativePath)).toEqual(assetRoot.expectedSources)
      }
      // #827: extensions/org-sse-rollout.ts deleted (the poll-only kill switch
      // and its module go with the deleted floors, D0) — 51 -> 50. #900:
      // extensions/model-change-orchestration.ts and model-observation.ts
      // move to src/extensionruntime/ (published seam, no longer an
      // extensions/ asset) — 50 -> 48. #964: extensions/bus-events-bounded-append.ts
      // added (shared bounded-append helper for the bus/events.jsonl and
      // acks-ledger producers) — 48 -> 49. The pi-loop deletion removed
      // extensions/team-default-monitor.ts (a loops-file writer) and
      // extensions/json-file-cache.ts (orphaned when the loop projection left
      // organization-live-work.ts took its only importer) — 49 -> 47.
      // extensions/org-send-replay.ts added (the `org_send` replay decision,
      // kept out of organization-intercom.ts so it is provable on its own and
      // so the intercom's quarantined export surface does not grow) — 47 -> 48.
      // The outbound messaging channel was deleted outright, taking its
      // extensions/ module (and its skill asset) with it — 48 -> 47. The memory
      // feature was deleted outright, taking extensions/organization-memory.ts
      // with it — 47 -> 46. The goal feature was deleted outright, taking
      // extensions/organization-live-work.ts (the goal/assignment card and
      // board projection) and extensions/organization-health-resolution.ts
      // (the goal-stall health resolution) with it — 46 -> 44. The sender-side
      // message-wake decision was deleted, taking
      // extensions/organization-runtime-wake.ts with it: the wake now rides on
      // the delivery, which is the only write scoped to the recipient — 44 -> 43.
      // The tavily-search capability was deleted on operator ruling, taking
      // extensions/tavily-search.ts with it — 43 -> 42. Provider/model
      // management was deleted outright, taking
      // extensions/zipbox-tribe-addons.ts (the custom-provider transport) with
      // it — 42 -> 41. The skills asset root was removed outright: the
      // browser, fal-ai, market-data and project-status-reporting skills were
      // deleted, and no package skill ships TypeScript any more, so
      // `tsconfig.capabilities.json` and its lint project went with them —
      // 41 -> 11, and `extensions/` is now the only moved asset root.
      // `company-stop.ts` was added to register `/stop` — 11 -> 12. The
      // number moves with its subject; the coverage claim itself is unchanged.
      expect(movedAssetSources).toHaveLength(12)

      const eslint = new ESLint({ cwd: PACKAGE_ROOT })
      const ignoredSources = (
        await Promise.all(
          movedAssetSources.map(async (source) =>
            (await eslint.isPathIgnored(source)) ? packageRelativePath(source) : undefined
          )
        )
      ).filter(Boolean)
      expect(ignoredSources).toEqual([])

      for (const assetRoot of assetRoots) {
        for (const source of assetRoot.sources) {
          const config = await eslint.calculateConfigForFile(source)
          expect(configuredProject(config)).toEqual([assetRoot.project])
        }
      }

      const results = await eslint.lintFiles(movedAssetSources)
      const parserFailures = results.flatMap((result) =>
        result.messages.filter(
          (message) =>
            message.fatal ||
            message.message.includes('was not found in any of the projects provided') ||
            message.message.includes('was not found by the project service')
        )
      )

      expect(results).toHaveLength(movedAssetSources.length)
      expect(parserFailures).toEqual([])
    },
    LINT_COVERAGE_BUDGET_MS
  )
})

#!/usr/bin/env node
// #3081. `tsc -p apps/<app>/tsconfig.json --noEmit` type-checks ZERO files and
// exits 0. Those configs are solution-style — `"files": []` plus `references` —
// and `-p` does not build references, so the command succeeds having asserted
// nothing. Measured: 0 files for apps/sandboxd, against 314 for apps/zipbox,
// whose config is a real one. The same command being sound in one workspace and
// vacuous in another is worse than uniformly broken: it teaches you to trust it.
//
// This guards the replacement (`bun run typecheck`) against becoming the thing it
// replaced. It answers one question — WOULD THIS INVOCATION CHECK ANY FILES —
// structurally, from the configs, so it costs milliseconds rather than a build.
//
// Deliberately not an exit-code check: "exits 0 having checked nothing" is
// precisely the failure, so asserting exit 0 would be vacuous in the same way as
// the defect.

import { readdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'

import ts from 'typescript'

// Read a tsconfig with TYPESCRIPT'S OWN parser, not a hand-rolled JSONC strip.
//
// The first version of this used regexes and silently corrupted most configs in
// the repo: `"@/*": ["./src/*"]` contains `/*` and `*/`, so a block-comment strip
// ate the property name and JSON.parse then failed — or, worse for a different
// input shape, would have parsed to something subtly wrong. Adding a fragile
// parser to a guard against fragile instruments is the joke writing itself; this
// defers to the compiler that owns the format.
export function readTsconfig(path) {
  const absolute = resolve(path)
  const { config, error } = ts.readConfigFile(absolute, ts.sys.readFile)
  if (error) {
    throw new Error(
      `cannot read ${absolute}: ${ts.flattenDiagnosticMessageText(error.messageText, ' ')}`
    )
  }
  return config ?? {}
}

// A config is SOLUTION-STYLE when it names no input files of its own and exists
// only to point at other projects. `tsc -p` on one of these checks nothing;
// `tsc -b` builds the references and checks everything.
export function isSolutionStyle(config) {
  const noInclude = !Array.isArray(config.include) || config.include.length === 0
  const noFiles = !Array.isArray(config.files) || config.files.length === 0
  const hasRefs = Array.isArray(config.references) && config.references.length > 0
  return noInclude && noFiles && hasRefs
}

// Would this invocation check at least one file?
//   -b on a solution config  -> yes (it builds the references)
//   -p on a solution config  -> NO. This is #3081.
//   either, on a config with include/files -> yes
export function wouldCheckFiles({ mode, config }) {
  if (isSolutionStyle(config)) {
    return mode === 'build'
  }
  const hasInclude = Array.isArray(config.include) && config.include.length > 0
  const hasFiles = Array.isArray(config.files) && config.files.length > 0
  return hasInclude || hasFiles
}

// Every project the root solution builds must itself resolve to real inputs —
// an empty `references` array, or a reference to another empty solution, would
// make `tsc -b` succeed while checking nothing.
export function resolveProjectInputs(configPath, seen = new Set()) {
  const absolute = resolve(configPath)
  if (seen.has(absolute)) {
    return 0
  }
  seen.add(absolute)
  const config = readTsconfig(absolute)
  const here = dirname(absolute)
  let count = 0
  if (Array.isArray(config.include) && config.include.length > 0) {
    count += config.include.length
  }
  if (Array.isArray(config.files) && config.files.length > 0) {
    count += config.files.length
  }
  for (const reference of config.references ?? []) {
    const target = join(here, reference.path)
    const candidate = target.endsWith('.json') ? target : join(target, 'tsconfig.json')
    count += resolveProjectInputs(candidate, seen)
  }
  return count
}

// `minimum` is optional and defaults to the original #3081 check (nonzero).
// #886: a solution-style root config's OWN `include` is always empty by
// definition (that is what makes it solution-style) — `assertMinimumRealFiles`
// below walks a config's own `include` patterns on disk, so calling it against
// a solution-style config always resolves to 0 real files and REFUSES TO RUN
// unconditionally, regardless of how much its referenced sub-projects cover.
// A floor on a solution-style config therefore has to apply to
// `resolveProjectInputs`'s aggregate (syntactic pattern) count instead, which
// is what this parameter does — it does not walk disk, so it cannot detect a
// referenced directory being deleted the way #848's real-file check can; it
// only detects a REFERENCE being dropped from the graph, which is #886's own
// class of regression (apps/cli falling back out of `tsconfig.json`).
export function assertNonVacuous(rootConfigPath, minimum = 0) {
  const config = readTsconfig(rootConfigPath)
  const problems = []
  if (!wouldCheckFiles({ mode: 'build', config })) {
    problems.push(`${rootConfigPath} would check no files even under 'tsc -b'`)
  }
  const inputs = resolveProjectInputs(rootConfigPath)
  if (inputs === 0) {
    problems.push(
      `${rootConfigPath} resolves to ZERO input patterns across its project graph — ` +
        `a typecheck over it would exit 0 having asserted nothing (#3081)`
    )
  } else if (inputs < minimum) {
    problems.push(
      `${rootConfigPath}'s project graph resolves to only ${inputs} input pattern(s), ` +
        `below the expected floor of ${minimum} — a reference was probably dropped from ` +
        `the graph (#886)`
    )
  }
  return { inputs, problems }
}

// #848. `assertNonVacuous` above catches #3081 (a solution-style config
// checked with `-p` instead of `-b`) by counting SYNTACTIC include patterns
// — but a plain (non-solution) config keeps its pattern count even when the
// directory those patterns name has been deleted. `tsconfig.legacy.json`'s
// `include: ["apps/cli/src/legacy/**/*.ts", "packages/piing/extensions/**/*.ts"]`
// still resolves syntactically even when a scan root has moved, so
// `assertNonVacuous` cannot prove that either shipping tree contributes files.
// This is the other, more literal
// meaning of vacuous: are there real files there. Deliberately a plain
// recursive directory walk, not a real glob engine — good enough for the
// `<dir>/**/*.ts` shape every plain config in this repo uses, and it costs
// milliseconds like its sibling check.
//
// #938: the suffix to match is derived from the include pattern itself
// (everything after its last `*`), not hardcoded to `.ts` — tsconfig.bun-
// check.json's `**/*.bun-check.mts` would otherwise always resolve to 0
// real files and REFUSE TO RUN unconditionally, the exact false-vacuous
// shape this file exists to prevent.
function suffixForPattern(pattern) {
  const lastStar = pattern.lastIndexOf('*')
  return lastStar === -1 ? '.ts' : pattern.slice(lastStar + 1)
}

// #938: a plain config's own `exclude` (a real, named entry) must subtract
// from the real-file count, or the floor would count a file `tsc` never
// actually checks -- a vacuity floor that overcounts hides the exact
// shrinkage it exists to catch.
function countFilesUnder(dir, suffix, excludedAbsolutePaths) {
  let entries
  try {
    entries = readdirSync(dir, { withFileTypes: true })
  } catch {
    return 0
  }
  let count = 0
  for (const entry of entries) {
    if (entry.name === 'node_modules' || entry.name === '.git' || entry.name === 'target') {
      continue
    }
    const path = join(dir, entry.name)
    if (entry.isDirectory()) {
      count += countFilesUnder(path, suffix, excludedAbsolutePaths)
    } else if (entry.name.endsWith(suffix) && !excludedAbsolutePaths.has(resolve(path))) {
      count += 1
    }
  }
  return count
}

function includeFileCounts(configPath) {
  const config = readTsconfig(configPath)
  const here = dirname(resolve(configPath))
  const excludedAbsolutePaths = new Set((config.exclude ?? []).map((entry) => resolve(here, entry)))
  return (config.include ?? []).map((pattern) => {
    const root = pattern.split('/**')[0].split('/*')[0]
    return {
      pattern,
      root,
      files: countFilesUnder(join(here, root), suffixForPattern(pattern), excludedAbsolutePaths)
    }
  })
}

// Assert a plain tsconfig's `include` globs resolve to at least `minimum`
// real `.ts` files on disk, not just `minimum` syntactic patterns.
export function assertMinimumRealFiles(configPath, minimum) {
  const total = includeFileCounts(configPath).reduce((sum, include) => sum + include.files, 0)
  if (total < minimum) {
    throw new Error(
      `${configPath}'s include patterns resolve to only ${total} real file(s), ` +
        `below the expected floor of ${minimum} — a scan root probably no longer exists (#848)`
    )
  }
  return total
}

// An aggregate floor alone can be masked by a large sibling tree. #785 adds a
// per-include floor so a moved package root cannot silently resolve to zero
// files while the legacy tree keeps the aggregate count green.
export function assertMinimumFilesForInclude(configPath, includePattern, minimum) {
  const matches = includeFileCounts(configPath).filter(({ pattern }) => pattern === includePattern)
  const total = matches.reduce((sum, include) => sum + include.files, 0)
  if (matches.length === 0 || total < minimum) {
    throw new Error(
      `${configPath}'s include '${includePattern}' resolves to only ${total} real file(s), ` +
        `below the expected floor of ${minimum} — a required scan root probably no longer exists (#848/#785)`
    )
  }
  return total
}

// Executed directly by scripts/typecheck.sh; importable by the test.
if (import.meta.url === `file://${process.argv[1]}`) {
  const rootConfig = process.argv[2] ?? 'tsconfig.json'
  const minFiles = process.argv[3] ? Number(process.argv[3]) : undefined
  // #751: `--include-floor` REPEATS. It was single-shot while one include
  // dominated the legacy config (a ~108-file `apps/cli/src/legacy/**` next to
  // a 19-file package-extension root), so an aggregate floor plus one
  // per-include floor could tell the two apart. E4's port inverted that — the
  // legacy root is now the SMALL one — and an aggregate floor can no longer
  // discriminate at all: any single number is either above the surviving
  // sibling (refusing a healthy tree) or below it (masking a deleted root).
  // Every include that must independently exist therefore names its own
  // floor, and the aggregate is left as a pure "did the whole leg go empty"
  // check.
  const includeFloors = []
  for (let index = 4; index < process.argv.length; index += 3) {
    const flag = process.argv[index]
    const pattern = process.argv[index + 1]
    const minimum = process.argv[index + 2] ? Number(process.argv[index + 2]) : undefined
    if (flag !== '--include-floor' || !pattern || minimum === undefined || Number.isNaN(minimum)) {
      console.error('usage: assert-typecheck-nonvacuous.mjs <tsconfig> [minimum] [--include-floor <include-pattern> <minimum>]...')
      process.exit(2)
    }
    includeFloors.push({ pattern, minimum })
  }
  // #886: a solution-style config (empty `include`/`files`, only `references`)
  // has no real files of its OWN to walk — `assertMinimumRealFiles` would
  // always resolve 0 and refuse unconditionally. Route it to the aggregate
  // project-graph floor instead; a plain config (like tsconfig.legacy.json)
  // keeps the original real-file-on-disk check.
  const solutionStyle = isSolutionStyle(readTsconfig(rootConfig))

  if (minFiles !== undefined && solutionStyle) {
    const { inputs, problems } = assertNonVacuous(rootConfig, minFiles)
    if (problems.length > 0) {
      console.error('[typecheck] REFUSING TO RUN — the typecheck would check almost nothing:')
      for (const problem of problems) {
        console.error(`  - ${problem}`)
      }
      process.exit(1)
    }
    console.log(`[typecheck] project graph resolves ${inputs} input pattern(s) — not vacuous`)
  } else if (minFiles !== undefined) {
    try {
      const total = assertMinimumRealFiles(rootConfig, minFiles)
      console.log(`[typecheck] ${rootConfig} resolves ${total} real file(s) — not vacuous`)
      for (const { pattern, minimum } of includeFloors) {
        const included = assertMinimumFilesForInclude(rootConfig, pattern, minimum)
        console.log(`[typecheck] ${rootConfig}'s ${pattern} resolves ${included} real file(s) — not vacuous`)
      }
    } catch (error) {
      console.error('[typecheck] REFUSING TO RUN — the typecheck would check almost nothing:')
      console.error(`  - ${error.message}`)
      process.exit(1)
    }
  } else {
    const { inputs, problems } = assertNonVacuous(rootConfig)
    if (problems.length > 0) {
      console.error('[typecheck] REFUSING TO RUN — the typecheck would check nothing:')
      for (const problem of problems) {
        console.error(`  - ${problem}`)
      }
      process.exit(1)
    }
    console.log(`[typecheck] project graph resolves ${inputs} input pattern(s) — not vacuous`)
  }
}

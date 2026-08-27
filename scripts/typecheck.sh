#!/usr/bin/env bash
# THE typecheck. CI runs `bun run typecheck`, which runs this file.
# Pattern ported from terminal scripts/typecheck.sh (#3081): `tsc -p` against
# a solution-style config type-checks NOTHING and exits 0, and cross-package
# imports need fresh dists — so the workspace half is turbo-build + `tsc -b`.
# The extensions half keeps packages/piing/extensions compiling: Pi loads them
# at runtime and no package imports them, so nothing else would.
# tests/** is NOT typechecked: the bun corpus is parked, is not
# shipping code, and its 351 ../src/** imports would block every tree move
# (ruling D15). It is not a compatibility surface — it is reference material
# with a written disposition per file (docs/testing/parked-suite-triage.json).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export NODE_OPTIONS="${NODE_OPTIONS:---max-old-space-size=8192}"

REFERENCE_COUNT="$(node -e 'console.log((JSON.parse(require("node:fs").readFileSync("tsconfig.json","utf8")).references ?? []).length)')"
if [ "$REFERENCE_COUNT" -gt 0 ]; then
  if [ ! -d node_modules/@chief ]; then
    echo "[typecheck] REFUSING TO RUN — workspace deps not wired (node_modules/@chief missing)." >&2
    echo "[typecheck] @chief/* imports would resolve elsewhere and tsc would report phantom errors." >&2
    echo "[typecheck] Run: bun install --frozen-lockfile" >&2
    exit 1
  fi
  # #907: DETECTION THRESHOLD, NOT HEADROOM — do not widen this to
  # "give it room" when the tree grows; a wide setting here defeats the one
  # regression it exists to catch. #886's class: a package with its own
  # tsconfig.json checked by NEITHER typecheck leg, proven by injecting a
  # real type error and watching this whole script exit 0. The floor of 15
  # sits against a real aggregate of 17 (four references: packages/piing,
  # packages/chiefing, packages/testing, apps/web) — sized so that any one of
  # them dropping back OUT of the graph fails loudly here rather than
  # silently narrowing what `tsc -b` below actually checks. The numbers were
  # 22-against-24 with apps/api in the graph and 15-against-20 with apps/cli;
  # each deletion lowers the real count, never the detection intent. If the
  # real count legitimately grows, raise this only enough to stay a few
  # below the true count — never round it up for comfort.
  # scripts/test/assert-typecheck-nonvacuous.mjs checks the other half: the
  # reference list equals the workspace members that HAVE a tsconfig.json.
  #
  # #825: this is NOT a vacuity floor like the two below. A vacuity floor
  # answers "did this leg check anything at all?" and wants to be low and
  # wide; this detection threshold answers "did we lose something specific?"
  # and must stay close to the number it is proving didn't move. Widening
  # this one to match the other floors' "half the real count" rule would
  # destroy the exact detection it exists for.
  node scripts/assert-typecheck-nonvacuous.mjs tsconfig.json 15
  echo "[typecheck] building workspace packages (suppresses the stale-dist false RED)"
  bun x turbo run build --filter='./packages/*' --output-logs=new-only
  echo "[typecheck] tsc -b tsconfig.json"
  bun x tsc -b tsconfig.json
fi

echo "[typecheck] Pi extensions: tsc --noEmit -p tsconfig.extensions.json"
# The extensions are loaded by Pi at runtime, not imported by any package, so
# nothing else type-checks them. VACUITY FLOOR, NOT AN INVENTORY: low and wide
# on purpose, so an ordinary deletion under this tree never forces an edit
# here. Re-derive the real count with
# `find packages/piing/extensions -name '*.ts' | wc -l` before assuming the
# number is tight; do not trust this comment's citation of it.
#
# #848 is the reason the floor exists at all: a plain (non-solution-style)
# config keeps its include-pattern count even when the directory those
# patterns name has been deleted, so the leg goes silently vacuous rather
# than red.
node scripts/assert-typecheck-nonvacuous.mjs tsconfig.extensions.json 8 --include-floor 'packages/piing/extensions/**/*.ts' 9
bun x tsc --noEmit -p tsconfig.extensions.json

echo "[typecheck] JavaScript corpus: tsc --noEmit -p tsconfig.guards.json"
# #1041. The two legs above read `.ts` and nothing else, by construction —
# so the 60 gates `node scripts/guard-count.mjs` derives, which are written
# in JavaScript, were typechecked by NOTHING. The corpus this repo trusts
# most (a `scripts/test/*.test.mjs` guard decides whether a change may land)
# was the one corpus no type checker read, and a typo in one was caught only
# when it ran, only if it ran, and only on the branch it took.
# `packages/eslinter` — 40 `.js` rule files that decide what every other
# package is allowed to compile — was invisible for the identical reason,
# and so were the root/package `eslint.config.mjs` files that load them.
# This leg reads all of it. First run found 107 real errors, including a
# `WebSocket` global that does not exist under node
# (browser-org-tools-check.mjs), an `import.meta.main` branch that can never
# execute under `node` (stub-import-guard.mjs), and `.length` read off a
# number (model-facing-copy.test.mjs).
#
# `strict: false` is a DELIBERATE, MEASURED setting, not a default nobody
# looked at. At `strict: true` the same corpus reports 1233 errors, 922 of
# which are TS7006/TS7031 "parameter implicitly has an 'any' type" — ~950
# JSDoc annotations across 87 files. That is a rewrite of the corpus, not a
# check of it, and it buys none of the failure class this leg exists for:
# misspelled identifiers, properties that do not exist on a real type, wrong
# arity, and globals that are not present in the runtime the file actually
# runs in are all caught at this setting, because `@types/node` supplies the
# types that matter. Raising it later is a separate, fundable piece of work;
# leaving the leg out entirely until someone funds it was the status quo,
# and the status quo is what this leg replaces.
#
# VACUITY FLOOR, NOT AN INVENTORY — same posture as the two legs above. The
# real aggregate is 150 files; the per-include floors exist because an
# aggregate alone cannot tell a deleted `packages/eslinter` from a deleted
# `scripts/test`, and each of those roots must independently keep resolving.
node scripts/assert-typecheck-nonvacuous.mjs tsconfig.guards.json 60 \
  --include-floor 'scripts/**/*.mjs' 40 \
  --include-floor 'packages/eslinter/**/*.js' 15
bun x tsc --noEmit -p tsconfig.guards.json

echo "[typecheck] scripts TypeScript: tsc --noEmit -p tsconfig.scripts.json"
# The sibling of the leg above, and the gap it closes is the same gap one
# extension deeper: `tsconfig.guards.json` reads `scripts/**/*.mjs`, and
# `scripts/**/*.ts` was read by NOTHING -- no typechecked project, no eslint
# scope. Proven the only way worth trusting: `const __proof: number = "no"`
# appended to scripts/tool-surface-artifact.ts, `bash scripts/typecheck.sh`,
# exit 0. That file is one the repo's own guards depend on, so the instrument
# did not cover its own subject. It found two real defects on its first run,
# both in that file: three dead keys handed to `installOrganizationIntercom`
# under a comment claiming "every background schedule off" (one of the three
# it named was not an option and was silently ignored), and `provider`/`model`
# on an `AgentProfile` that has never had either -- leftovers of deleted
# provider/model management.
#
# Its own config, NOT three words added to tsconfig.guards.json's include:
# that config is `strict: false` for a measured JavaScript-corpus reason (see
# its comment above -- 1233 errors, 922 implicit-any), and TypeScript source
# must not inherit a concession made for JavaScript. Measured both ways:
# 22 errors across 7 files through the guards config, 20 of them manufactured
# by that config; 2 through this one, both real.
#
# VACUITY FLOOR, NOT AN INVENTORY -- same posture as the legs above. Six real
# files today; the floor is well below that so an ordinary deletion here never
# forces an edit, and well above the 0 a moved `scripts/` root would produce.
node scripts/assert-typecheck-nonvacuous.mjs tsconfig.scripts.json 4
bun x tsc --noEmit -p tsconfig.scripts.json

# #938's bun-check driver leg was REMOVED here, not disabled or floored to 0.
# It typechecked `packages/piing/test/*.bun-check.mts` -- cross-language Bun
# drivers that sit outside every other leg's include glob. Two existed; the
# E2E corpus deletion took one, and the pi-loop deletion took the other
# (LoopDeliveryCompat.bun-check.mts), so the population is now genuinely zero
# and there is no `.bun-check.mts` file left in the tree.
#
# Lowering the leg's floor to 0 was the one move its own comment forbade
# ("never lower it to make room for a deletion") -- correctly, because a leg
# that runs while observing nothing reports success it did not earn. A leg
# with no subject is deleted instead. If a bun-check driver that IS checkable
# from its repo path ever appears, restore the leg with a floor matching its
# real count.

# The capabilities leg (`tsconfig.capabilities.json`) was REMOVED here on the
# same rule as the bun-check leg above, and for the same reason: its subject is
# gone. It typechecked `packages/piing/skills/**/*.ts` -- the runtime sources
# package skills shipped alongside their SKILL.md. The skill set is prose and
# frontmatter now; the `browser`, `fal-ai`, `market-data` and
# `project-status-reporting` skills that carried every one of those `.ts` files
# were deleted, so the include pattern resolves to zero real files.
#
# Its floor of 15 could not be lowered to 0 -- "never lower it for a deletion's
# sake" is the same rule the bun-check note states -- so the leg is deleted, and
# `tsconfig.capabilities.json` with it. If a package skill ever ships TypeScript
# again, restore the config and the leg with a floor matching its real count.

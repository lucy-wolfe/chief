#!/usr/bin/env bash
# #993/#994 regression leg: extracts the FIRST fenced ```bash block in
# CONTRIBUTING.md — the from-source setup — and runs it, verbatim, from a
# clean clone. That document is the specification here, not a paraphrase of
# one: if a step is missing, mis-ordered, or wrong, this leg goes red and
# names it, and it cannot silently drift from the document it guards because
# it never transcribes the commands by hand (the same derive-not-transcribe
# rule scripts/gate-matrix-legs.mjs applies to guard lists, applied to docs).
#
# REPOINTED FROM README.md (open-source launch). It read README's first bash
# block for as long as that block WAS the build-from-source sequence. The
# open-source rewrite split the two audiences: README's quick start is now the
# USER path — a curl one-liner that unpacks a prebuilt release and never
# touches bun, cargo, or this checkout — and the contributor path moved whole
# into CONTRIBUTING.md's "Set up a clean machine". This leg's subject is the
# CONTRIBUTOR sequence: everything below (#993's module graph, the frozen-
# lockfile detector, the workspace-barrel checkpoint) is about a clone that has
# to build. Repointing it is following its subject, not lowering its bar. Left
# on README it went red on a block that no longer contains `bun run release`,
# which is exactly the refusal the needle floor below exists to produce.
#
# WHAT THIS LEG CHECKS, STATED AS A PROPERTY RATHER THAN A COMMAND:
# a clean clone that follows the documented setup must be left with a
# RESOLVABLE MODULE GRAPH — `bun install` must have run its postinstall
# workspace build, so `@chief/chiefing` and every sibling package resolve
# from source-only checkouts. That property, not any particular entry point,
# is #993: `bun install` alone left packages source-only and the launcher
# died with `Cannot find module '@chief/chiefing'`, fifteen batches behind a
# gate reporting 9/9.
#
# REPOINTED (root script-surface cleanup). This leg used to prove that
# property by running `bun start` outside tmux and asserting it reached
# `apps/cli/src/legacy/foundation/preflight.ts`'s `outside-tmux` refusal.
# BOTH halves of that checkpoint are gone: #751/P0 deleted
# `apps/cli/src/legacy` (so the refusal text it grepped for cannot be
# printed by anything), and the root `start` script was deleted with the
# rest of the alias drawer. The leg was already red before either the
# README or this file was touched — its needle floor demands `bun install`,
# `tmux new-session` and `bun start` in the extracted block, and README's
# quick-start block contains none of the three.
#
# REPOINTED AGAIN (P3), taking that instruction: the checkpoint no longer
# runs an entry point at all, because there is no longer a TypeScript one to
# run — P3 deleted `apps/cli`, and the operator surface is the Rust binary.
# It now imports the workspace barrels (`@chief/chiefing`, `@chief/piing`,
# `@chief/testing`) directly, from the clone, with bun. That is the property
# stated as itself rather than through a proxy: #993 was `Cannot find module
# '@chief/chiefing'` after a `bun install` that had not run its postinstall
# workspace build, and these packages' `exports` maps resolve against
# `dist/`, so a source-only checkout fails exactly here and a built one
# prints the sentinel. It is also strictly broader than the old checkpoint,
# which resolved whatever ONE app happened to import.
#
# Two lines are NOT executed as written, and these are the only deliberate
# departures from "verbatim":
#   - `cd <repo>`               — the clone destination IS the cwd already.
#   - `git clone <url>`         — this leg tests THIS tree's committed state,
#     and running the documented clone would fetch `main` from GitHub and
#     test that instead, over the network, in a job that is otherwise
#     hermetic. The clone at the top of this script is that step, performed
#     against the local source root.
#
# IT TESTS COMMITTED STATE, NOT YOUR WORKING TREE. The clone below is a
# `git clone` of the source root, so an uncommitted fix reads to this leg as
# still-broken. That is deliberate — a clean-install path a user follows is
# whatever is committed — but it surprises everyone once, so it is stated
# here rather than learned.
#
# IT IS A LIVE LOCKFILE DETECTOR, and that property must survive any future
# rewrite. Because the clone is fresh and `bun run release` starts its shared
# preparation with `bun install --force --frozen-lockfile`, a `bun.lock` that disagrees with the
# committed manifests fails here with "lockfile had changes, but lockfile is
# frozen" — proven on a build host, where regenerating removed 31 lines with
# zero additions. Nothing else in the gate matrix installs from a clean
# clone, so nothing else can see that class of defect at all.
#
# WHAT THIS GREEN PROVES, AND WHAT IT DOES NOT: reaching the CLI's own
# resolved barrel imports prove the workspace build ran and every package's
# published entry resolves — the exact property #993 broke, and
# the earliest deterministic checkpoint reachable without a live tmux server
# or a model provider credential on the build host. It does NOT prove that
# a company boots, that Pi is reachable, or that `beacond`/chiefd's runtime
# is healthy; this leg does not test those and never did.
#
# Cost is NOT a fixed number: it depends entirely on how much the extracted
# block builds (a Rust release build is not cheap) and on host cache state.
# See the printed COST line for this run's actual, host-local measurement —
# do not carry it to a different or colder host as a promise.
#
# TWO MODES, two different jobs, deliberately kept separate rather than one
# leg trying to be both:
#   --mode fast (default) — runs shared release preparation, then the checkpoint.
#     CONTRIBUTING's install line is `bun run release`, which delegates
#     directly to `bun scripts/release-chiefd.ts`. Fast mode derives that entry
#     from package.json and adds `--prepare-only`, so it runs the same forced
#     frozen install and Pi attestation while omitting only Cargo and
#     publication. It announces the substitution at runtime, then runs the
#     checkpoint. ~6-11s on a cache-warm host. This is the mode meant for
#     gate-matrix.sh and ci.yml.
#   --mode full — runs the ENTIRE extracted block verbatim (only `cd` and
#     `git clone` are skipped, as always). This is the one that tests the
#     DOCUMENT, not just the defect: if the documented install line, its
#     flags, or its ordering are wrong, only full mode notices. Not cheap (a cold Rust
#     release build dominates) and not meant to gate every batch — run on
#     demand and before any release claim.
#   --mode auto — what gate-matrix.sh actually invokes. Resolves to full
#     when this batch's own commit (HEAD vs. its first parent) touches
#     CONTRIBUTING.md or root package.json — a deliberate, transcribed
#     trigger list, not derived, because it names exactly full mode's actual
#     job: proving the DOCUMENTED SEQUENCE works. Those two files ARE the
#     sequence — one states the steps, the other defines what they do.
#     Deliberately narrow: full mode's own cargo build does NOT warm the
#     gate's main release-build cache (measured live — cargo fingerprints by
#     absolute source path, and this leg always builds from a throwaway
#     clone, so the leg's 225s is fully additive on top of the main build,
#     every time it fires, not overlapping work). A wider trigger (scripts/,
#     any Cargo.toml — the original design) would have fired on most
#     batches, turning a one-time-feeling cost into a recurring one.
#
# Usage: scripts/clean-install-smoke.sh [--source <repoRoot>] [--mode fast|full|auto]
# Exit 0 = the checkpoint resolved every workspace barrel, and (full mode
# only) every other extracted step exited 0. Exit 1 = any run step failed, or
# the checkpoint crashed (module resolution or otherwise).
set -uo pipefail

SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="fast"
while [ $# -gt 0 ]; do
  case "$1" in
    --source) SOURCE_ROOT="$2"; shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done
case "$MODE" in
  fast|full) : ;;
  auto)
    FULL_TRIGGER_PATHS='^(CONTRIBUTING\.md|package\.json)$'
    TRIGGERED_BY_DIFF=0
    # #1000: HEAD~1..HEAD only sees the tip commit. A linearized/assembled
    # batch's HEAD is one commit whose parent is the PRE-BATCH tip, but
    # gate-matrix.sh can also run mid-assembly against a branch carrying the
    # batch's several original commits unsquashed — HEAD~1 only sees the
    # last of those, silently dropping an earlier CONTRIBUTING.md/package.json
    # touch from the same batch. Diff the whole batch range instead: from
    # where this branch actually diverged from the integration branch, to
    # HEAD. GATE_MATRIX_DIFF_BASE lets a caller (the batch assembler) name
    # that divergence point explicitly; otherwise it's the merge-base with
    # origin/revamp/monorepo, falling back to HEAD~1 only if neither
    # resolves (e.g. no origin remote in a throwaway clone).
    DIFF_BASE="${GATE_MATRIX_DIFF_BASE:-}"
    if [ -z "$DIFF_BASE" ]; then
      DIFF_BASE="$(git -C "$SOURCE_ROOT" merge-base HEAD origin/revamp/monorepo 2>/dev/null || true)"
    fi
    [ -z "$DIFF_BASE" ] && DIFF_BASE="HEAD~1"
    if CHANGED="$(git -C "$SOURCE_ROOT" diff --name-only "$DIFF_BASE" HEAD 2>/dev/null)"; then
      if grep -qE "$FULL_TRIGGER_PATHS" <<<"$CHANGED"; then
        TRIGGERED_BY_DIFF=1
        echo "[clean-install-smoke] auto mode: batch range ($DIFF_BASE..HEAD) touches a full-mode trigger path:"
        grep -E "$FULL_TRIGGER_PATHS" <<<"$CHANGED" | sed 's/^/  | /'
      fi
    fi
    if [ "$TRIGGERED_BY_DIFF" -eq 1 ]; then
      MODE="full"
    else
      MODE="fast"
      echo "[clean-install-smoke] auto mode: neither CONTRIBUTING.md nor package.json changed in this commit — fast"
    fi
    ;;
  *) echo "--mode must be 'fast', 'full', or 'auto', got '$MODE'" >&2; exit 1 ;;
esac

WORKDIR="$(mktemp -d -t clean-install-smoke-XXXXXX)"
FRESH_HOME="$(mktemp -d -t clean-install-smoke-home-XXXXXX)"
CLONE="$WORKDIR/repo"
cleanup() { rm -rf "$WORKDIR" "$FRESH_HOME"; }
trap cleanup EXIT

echo "[clean-install-smoke] cloning $SOURCE_ROOT -> $CLONE (working tree only, no reused node_modules/dist)"
git clone --quiet "$SOURCE_ROOT" "$CLONE" || { echo "[clean-install-smoke] FAIL: git clone failed"; exit 1; }

SETUP_DOC="$CLONE/CONTRIBUTING.md"
[ -f "$SETUP_DOC" ] || { echo "[clean-install-smoke] FAIL: CONTRIBUTING.md not found in clone"; exit 1; }

# Extract the FIRST ```bash ... ``` fenced block — the from-source setup.
BLOCK="$(awk '/^```bash$/{flag=1; next} /^```$/{if(flag){exit}} flag{print}' "$SETUP_DOC")"
if [ -z "$BLOCK" ]; then
  echo "[clean-install-smoke] FAIL: could not extract a \`\`\`bash setup block from CONTRIBUTING.md — the extraction regex and the document have drifted apart"
  exit 1
fi
echo "[clean-install-smoke] mode: $MODE"
echo "[clean-install-smoke] extracted setup block:"
echo "$BLOCK" | sed 's/^/  | /'

# Sanity floor on the extraction itself: it must still contain the install
# command this leg's fast-mode substitution reasons about. If the document
# changes shape enough that it no longer appears, the departures above are
# stale reasoning, not a stale leg — refuse rather than silently run a block
# that no longer matches this file's own comments. (This floor has now fired
# twice, and both times it was right: once when it demanded `bun start`, which
# README's quick-start had stopped naming, and once when README's quick-start
# became the curl installer and stopped naming `bun run release` at all. The
# second firing is what repointed this leg at CONTRIBUTING.md.)
for needle in "bun run release"; do
  if ! grep -qF -- "$needle" <<<"$BLOCK"; then
    echo "[clean-install-smoke] FAIL: the extracted setup block no longer contains '$needle' — this leg's departures from verbatim execution (see header) reason about a block shape that has changed. Update the leg, not just the document."
    exit 1
  fi
done

RUN_LOG="$WORKDIR/run.log"
INSTALL_START=$(date +%s)
: > "$RUN_LOG"
STEP_FAILED=0
while IFS= read -r line; do
  trimmed="$(echo "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
  [ -z "$trimmed" ] && continue
  # #1001: classify on the command with any trailing `# ...` comment
  # stripped, never on $trimmed itself. A documented line like
  # `bun install  # installs deps` still executes correctly (bash's own
  # comment handling takes care of that), but an exact-string classifier
  # compared against $trimmed treats it as an unrecognized, skippable line
  # in fast mode — silently dropping the one step fast mode exists to run,
  # then reporting the exact #993 module-resolution defect this leg was
  # built to catch. classify is for "what step is this", never for "what do
  # we execute" — $trimmed (or $line) still runs verbatim.
  classify="$(sed -E 's/[[:space:]]+#.*$//' <<<"$trimmed")"
  case "$classify" in
    cd\ *) echo "[clean-install-smoke] skipping '$trimmed' — clone dir is already cwd"; continue ;;
    git\ clone\ *) echo "[clean-install-smoke] skipping '$trimmed' — the clone at the top of this leg IS that step, performed against this tree rather than fetching main from GitHub"; continue ;;
  esac
  # What actually runs. Defaults to the documented line VERBATIM; fast mode is
  # the only thing that ever changes it, and it says so out loud when it
  # does. Skipping is the risky direction here, not running an extra step,
  # so an uncertain classification must run rather than skip: only skip when
  # classify is confidently NOT a step the checkpoint needs.
  RUN_CMD="$trimmed"
  if [ "$MODE" = "fast" ]; then
    case "$classify" in
      "bun install") : ;;
      "bun run release")
        # DERIVED, never transcribed: the substitution is the `release`
        # script's own shared entry with its preparation-only flag. The entry
        # owns the forced frozen install and Pi attestation; fast smoke stops
        # there because Cargo and publication are not part of this checkpoint.
        RUN_CMD="$(node -e '
          const { readFileSync } = require("node:fs");
          const release = (JSON.parse(readFileSync(process.argv[1], "utf8")).scripts || {}).release || "";
          if (!/^bun scripts\/release-chiefd\.ts$/.test(release.trim())) process.exit(3);
          process.stdout.write(`${release.trim()} --prepare-only`);
        ' "$CLONE/package.json")" || {
          echo "[clean-install-smoke] FAIL: package.json's \`release\` script no longer names the shared release entry, so fast mode cannot derive its preparation command. Run --mode full, or teach this leg the new shape — do not guess."
          exit 1
        }
        echo "[clean-install-smoke] fast mode: substituting '$RUN_CMD' for '$trimmed' — the release entry's shared forced-install and attestation path. Cargo and publication are not needed to reach the checkpoint; run --mode full to execute the documented command whole."
        ;;
      *)
        echo "[clean-install-smoke] fast mode: skipping '$trimmed' — not required to reach the module-resolution checkpoint; run --mode full to execute it"
        continue
        ;;
    esac
  fi
  echo "[clean-install-smoke] running: $RUN_CMD"
  if ! (cd "$CLONE" && HOME="$FRESH_HOME" env -u TMUX bash -c "$RUN_CMD" >> "$RUN_LOG" 2>&1); then
    STEP_FAILED=1
    echo "[clean-install-smoke] FAIL: documented setup step failed: $trimmed"
    tail -n 40 "$RUN_LOG"
    break
  fi
done <<<"$BLOCK"
INSTALL_END=$(date +%s)

if [ "$STEP_FAILED" -ne 0 ]; then
  exit 1
fi

echo "[clean-install-smoke] COST: documented install steps took $((INSTALL_END - INSTALL_START))s on this host ($(hostname), $(date -u +%FT%TZ)) — host- and cache-state-dependent, not a portable figure"

# The checkpoint. Import every workspace barrel whose `exports` map resolves
# against `dist/`: that resolution is what `bun install`'s postinstall
# workspace build produces, and its absence is #993 verbatim. A sentinel on
# stdout is the pass shape; anything else is a failure to resolve.
# INSIDE the clone, deliberately: bun resolves `@chief/*` from the importing
# file's own directory upward, so a checkpoint parked in the scratch dir would
# fail to resolve on a perfectly good tree — measured, not assumed.
CHECKPOINT="$CLONE/resolve-workspace-barrels.ts"
cat > "$CHECKPOINT" <<'CHECKPOINT_EOF'
import '@chief/chiefing'
import '@chief/piing'
import '@chief/testing'

// eslint-disable-next-line no-console -- this file is a smoke checkpoint run by bash, not application code
console.log('WORKSPACE MODULE GRAPH RESOLVED')
CHECKPOINT_EOF
echo "[clean-install-smoke] checkpoint: import @chief/{chiefing,piing,testing} from the fresh clone — must resolve, not crash on module resolution"
START_LOG="$WORKDIR/start.log"
(cd "$CLONE" && HOME="$FRESH_HOME" env -u TMUX timeout 60 bun ./resolve-workspace-barrels.ts > "$START_LOG" 2>&1)
START_STATUS=$?

# #1001: check the PASS shape first. The module-resolution grep below reads
# the WHOLE combined log, and a tree that actually resolved cleanly can
# still incidentally print a benign "cannot find module" line (an optional
# peer-dependency probe, a plugin lookup) on its way to the sentinel --
# that is a READ, not the #993 crash this leg exists to catch, and grepping
# for the string before checking whether the run actually reached its
# expected success shape would report the #993 defect verbatim against a
# tree that never had it.
if [ "$START_STATUS" -eq 0 ] && grep -qF "WORKSPACE MODULE GRAPH RESOLVED" "$START_LOG"; then
  TOTAL_END=$(date +%s)
  echo "[clean-install-smoke] PASS: every workspace barrel resolved from the fresh clone"
  echo "[clean-install-smoke] COST: total wall time $((TOTAL_END - INSTALL_START))s on this host"
  exit 0
fi

if grep -qi "cannot find module\|cannot find package\|module not found\|MODULE_NOT_FOUND" "$START_LOG"; then
  echo "[clean-install-smoke] FAIL: a workspace barrel did not resolve — CONTRIBUTING's own setup sequence does not leave a runnable tree"
  tail -n 40 "$START_LOG"
  exit 1
fi

echo "[clean-install-smoke] FAIL: the checkpoint did not print its sentinel (exit=$START_STATUS)"
tail -n 40 "$START_LOG"
exit 1

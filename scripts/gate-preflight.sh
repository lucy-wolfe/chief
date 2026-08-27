#!/usr/bin/env bash
# #934: THE GATE DRIVER'S OWN PRECONDITIONS, FAIL-CLOSED.
#
# Every other guard in this repo checks the tree. This one checks the
# *instrument* — the merger/seat driver that runs the matrix. It exists
# because four defects in one driver went unnoticed for nine landings, and
# every one of them made the matrix WEAKER than CI while still reporting a
# green that looked identical to a real one.
#
# The load-bearing rule: a matrix that cannot reproduce CI's conditions must
# REFUSE TO RUN, not run and report. A degraded run is not a partial result;
# it is a result about a different question.
#
# Usage:  bash scripts/gate-preflight.sh <repo-root> [pre|post|all]
#   pre   (before the debug test build): CI set, disk sufficient, the repo's
#         cargo runner exists. Cheap; fails fast, before a multi-minute
#         build starts.
#   post  (after binaries are provisioned in-repo): debug test binaries
#         present+executable, guard-count's [shell-gate] section non-empty
#         when the tool exists, and — when CARGO_CACHE_STATE_SINCE_MS is
#         set — the #941 cache-state stamp is present and fresh.
#   all   (default, omitted): every check above, in order. Backward
#         compatible with every existing caller/test that invokes this
#         script with just a repo root.
# Exit 0 = safe to gate. Exit 1 = refused, with the reason on stdout.
#
# WHY THE SPLIT: a disk-floor or CI-unset check that only runs AFTER a
# multi-minute debug build cannot protect that build — it just reports,
# after the fact, that the run it already spent minutes on should never
# have started. The binary-presence check is the opposite: it structurally
# CANNOT run before the build, since nothing exists yet to check. Two
# invocations of the same script, different arms — not two preflights.
#
# One override exists, ONLY for this script's own tests:
#   GATE_PREFLIGHT_MIN_FREE_GB   (default 25)
# The seam is the THRESHOLD, never the check. There is deliberately no
# skip/disable flag: a fail-closed guard with an off switch is a warning with
# extra steps, and every defect this guard exists to catch was silent.
set -u

ROOT="${1:-}"
PHASE="${2:-all}"
if [ -z "$ROOT" ] || [ ! -d "$ROOT" ]; then
  echo "REFUSING TO GATE: repo root '${ROOT}' is not a directory."
  exit 1
fi
case "$PHASE" in
  pre|post|all) ;;
  *)
    echo "REFUSING TO GATE: unknown phase '${PHASE}' — must be pre, post, or all."
    exit 1
    ;;
esac

run_pre_checks() {
  # 1. CI must be set. chiefdBinaryTestGate and the chiefd-e2e precondition
  #    both branch on it: unset, they SKIP; set, they THROW. With CI unset,
  #    41 tests left the totals silently and a `2537 passed / 0 failed` line
  #    contained a term that certified nothing. This is not a degraded run;
  #    its green would mean nothing.
  #
  #    THIS CHECK MUST NEVER BE SATISFIED BY THE DRIVER SUPPLYING CI ITSELF.
  #    A driver that exports CI=1 makes this refusal unfalsifiable in the
  #    very run it exists to protect — the condition would hold by
  #    construction, not by the caller's invocation. CI is read from the
  #    CALLER's environment only; if it's unset, refuse, don't supply it.
  if [ -z "${CI:-}" ]; then
    echo "REFUSING TO GATE: CI is unset. chiefdBinaryTestGate would skip instead of throw, and tests would vanish from the totals rather than fail. This is not a degraded run; its green would mean nothing. Invoke as: CI=1 bash scripts/gate-preflight.sh <root>"
    exit 1
  fi

  # 2. The repo's own cargo runner must exist. A bare `cargo test --workspace`
  #    truncates at the first failing suite and reports a count that is not
  #    the tree's — 57 suites / 1973 tests against a real 78 / 2537.
  if [ ! -f "$ROOT/scripts/cargo-test-workspace.sh" ]; then
    echo "REFUSING TO GATE: scripts/cargo-test-workspace.sh is missing. Refusing to fall back to a fail-fast 'cargo test --workspace', whose count is a fact about when the runner stopped, not about the tree."
    exit 1
  fi

  # 3. Disk. A matrix needs ~16G; a disk-truncated red cannot report the
  #    failure it is nominally responsible for, so it masquerades as a code
  #    result. Checked BEFORE the build so a doomed run never starts.
  MIN="${GATE_PREFLIGHT_MIN_FREE_GB:-25}"
  # The read is POSIX: `-k` fixes the block size at 1024 bytes and `-P` fixes
  # the output at the POSIX layout -- exactly one line per filesystem, with
  # available as field 4. Both hold identically on macOS and on GNU/Linux, so
  # the check does real work on a developer's Mac and on a build host alike.
  # This was `df -BG --output=avail`, and BOTH of those flags are GNU
  # coreutils only: macOS df rejects `-B` outright, FREE came back empty, and
  # the check below refused 100% of the time with "only unknownG free" on a
  # host with 70G available -- a portability gap wearing a disk shortage's
  # clothing, which made the guard measure nothing on half the fleet.
  # It still FAILS CLOSED by design: if df cannot report on this path the
  # available field is empty, the -z test refuses, and nothing is skipped.
  # Written as an `if` rather than `[ -n .. ] && FREE=..` on purpose: the `&&`
  # form evaluates to non-zero exactly when df could not be read, which under a
  # future `set -e` would EXIT here instead of reaching the refusal below --
  # same outcome today, silently wrong the day someone hardens this script.
  FREE_KB=$(df -kP "$ROOT" 2>/dev/null | awk 'NR==2 {print $4}' | tr -dc '0-9')
  FREE=""
  if [ -n "$FREE_KB" ]; then
    FREE=$((FREE_KB / 1024 / 1024))
  fi
  if [ -z "$FREE" ] || [ "$FREE" -lt "$MIN" ]; then
    echo "REFUSING TO GATE: only ${FREE:-unknown}G free at $ROOT, need >= ${MIN}G. Release landed gate directories before running a matrix you cannot fit."
    exit 1
  fi
}

run_post_checks() {
  # 4. The debug test binaries must sit where ci.yml's download-artifact step
  #    puts them — the IN-REPO path. A packet-owned CARGO_TARGET_DIR is
  #    correct for isolation and is NOT where consumers look; provision
  #    both, as CI does. Structurally cannot run before the build: nothing
  #    exists yet to check.
  #    #1041: THREE binaries, not two. `chiefd` was missing from this
  #    list while the message below claimed to match ci.yml, and ci.yml's
  #    test-unit job chmod +x's `chiefd`, `chiefd` AND `beacond`.
  #    `chiefd` is the operator client; `chiefd` is the backend
  #    `resolveChiefdDaemonBinary` boots, so a tree with only the first two
  #    fails 13 suites with "chiefd binary not found at
  #    .../release/chiefd" — which is a provisioning fact wearing a
  #    regression's clothing, and precisely what this arm exists to catch
  #    BEFORE the suites run.
  for bin in chief chiefd beacond; do
    p="$ROOT/apps/chiefd/target/debug/$bin"
    if [ ! -x "$p" ]; then
      echo "REFUSING TO GATE: $p is missing or not executable. ci.yml's test-unit job downloads all three debug test binaries to this exact path and chmod +x's them before vitest; a matrix without them is stricter than CI in one direction and blinder in another."
      exit 1
    fi
  done

  # 5. [#941] guard-count's own [shell-gate] section, when the tool is
  #    present in this tree (a synthetic fixture without it is not this
  #    check's business — the tool's ABSENCE is not this script's failure
  #    mode, its EMPTINESS on a real tree is). A guard file can land wired
  #    into no CI job and nothing notices unless something checks both
  #    conventions the derivation reports, not just that a count exists.
  GUARD_COUNT_TOOL="$ROOT/scripts/guard-count.mjs"
  if [ -f "$GUARD_COUNT_TOOL" ]; then
    GUARD_COUNT_OUT="$(node "$GUARD_COUNT_TOOL" 2>&1)"
    GUARD_COUNT_STATUS=$?
    if [ "$GUARD_COUNT_STATUS" -ne 0 ]; then
      echo "REFUSING TO GATE: scripts/guard-count.mjs exited $GUARD_COUNT_STATUS. A driver that cannot even enumerate the guards it is about to run should not claim to have run them."
      echo "$GUARD_COUNT_OUT"
      exit 1
    fi
    if ! grep -q '^DERIVED_GUARD_COUNT:' <<<"$GUARD_COUNT_OUT"; then
      echo "REFUSING TO GATE: scripts/guard-count.mjs produced no DERIVED_GUARD_COUNT line — the combined figure this driver reports would be a fact about nothing."
      exit 1
    fi
    if ! grep -q '\[shell-gate\]' <<<"$GUARD_COUNT_OUT"; then
      echo "REFUSING TO GATE: scripts/guard-count.mjs reported an EMPTY [shell-gate] section. CI-wired shell gates (typecheck.sh, cargo-test-workspace.sh, cargo-check-macos.sh) are a real, non-empty category of guard; an empty section here means the derivation itself is broken, not that the category is genuinely empty."
      exit 1
    fi
  fi

  # 6. [#941] cache state, ONLY when the caller asks
  #    (CARGO_CACHE_STATE_SINCE_MS set) — this arm only applies once a
  #    cargo-cache-state.mjs "build" call has had a chance to stamp the
  #    resolved CARGO_TARGET_DIR.
  if [ -n "${CARGO_CACHE_STATE_SINCE_MS:-}" ]; then
    CACHE_STATE_TOOL="$ROOT/scripts/cargo-cache-state.mjs"
    if [ ! -f "$CACHE_STATE_TOOL" ]; then
      echo "REFUSING TO GATE: CARGO_CACHE_STATE_SINCE_MS is set but scripts/cargo-cache-state.mjs is missing."
      exit 1
    fi
    if ! node "$CACHE_STATE_TOOL" assert --root "$ROOT" --since "$CARGO_CACHE_STATE_SINCE_MS"; then
      exit 1
    fi
  fi
}

case "$PHASE" in
  pre)
    run_pre_checks
    echo "gate-preflight (pre): OK — CI set, cargo-test-workspace.sh present, disk sufficient."
    ;;
  post)
    run_post_checks
    echo "gate-preflight (post): OK — debug test binaries provisioned in-repo, guard-count non-vacuous, cache-state fresh (if requested)."
    ;;
  all)
    run_pre_checks
    run_post_checks
    echo "gate-preflight: OK — CI set, debug test binaries provisioned in-repo, cargo-test-workspace.sh present, disk sufficient, guard-count non-vacuous, cache-state fresh (if requested)."
    ;;
esac
exit 0

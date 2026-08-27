#!/usr/bin/env bash
# THE sanctioned wrapper for `cargo test --workspace` (#857) — mirrors
# scripts/cargo-check-macos.sh's role as the ONE sanctioned check; do not
# hand-roll a second one.
#
# Gating #814, a plain `cargo test --workspace` reported "1816 passed,
# 0 failed" while a single e2e test had actually failed and cargo's
# fail-fast halted the run — 544 tests behind it never executed, and the
# summary for that partial run read exactly like a complete one. Two
# distinct causes produce the same invisible symptom:
#   1. fail-fast truncation       -> closed outright by --no-fail-fast below
#   2. a crate that fails to build -> --no-fail-fast does NOT help: that
#      crate contributes zero "test result:" lines either way
# So this always runs --no-fail-fast AND asserts a floor on the total
# executed count (scripts/cargo-test-floor.mjs) afterward — the second
# check is what catches cause 2, and is the same instrument #848's
# assertMinimumRealFiles and sql-only-state's MINIMUM_WRITE_SITES already
# proved for their own domains, pointed at test execution.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# CONTAINMENT: this lane drives REAL tmux, and it must not be able to reach a
# server it did not start. Eleven `kill-server` call sites live in it, and the
# socket a product path resolves can come from the ambient `$TMUX` rather than
# from any fixture (see `scripts/with-private-tmux.sh` for the full mechanism).
#
# Delegated rather than inlined. This block used to be a copy of the wrapper's
# logic, and a rule that lives in two places rots in two places — the same
# argument that made the guard corpus derived rather than hand-listed. Re-execs
# through the wrapper exactly once; the guard below the `if` keeps that from
# recursing.
if [ -z "${CHIEF_PRIVATE_TMUX:-}" ]; then
  export CHIEF_PRIVATE_TMUX=1
  exec bash "$ROOT/scripts/with-private-tmux.sh" bash "${BASH_SOURCE[0]}" "$@"
fi

# CI supplies one package group per matrix runner. Keep this stable wrapper as
# the gate name so the existing shell-gate inventory and branch checks still
# point to one sanctioned entry point. Local callers without the matrix
# variables retain the complete workspace command below.
if [[ -n "${CI_CARGO_PACKAGES:-}" && -n "${CI_CARGO_MEMBERS:-}" ]]; then
  exec bash "$ROOT/scripts/cargo-test-workspace-shard.sh" "$@"
fi

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

cargo test --workspace --locked --no-fail-fast \
  --manifest-path "$ROOT/apps/chiefd/Cargo.toml" "$@" 2>&1 | tee "$LOG"
CARGO_STATUS="${PIPESTATUS[0]}"

# The floor check is unconditional, even when cargo itself already failed:
# a run that both failed AND executed too few tests should report the
# truncation cause too, not just "some test failed" — that's the exact
# ambiguity #857 exists to remove.
if ! node "$ROOT/scripts/cargo-test-floor-lib.mjs" "$LOG"; then
  exit 1
fi

exit "$CARGO_STATUS"

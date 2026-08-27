#!/usr/bin/env bash
# Binary freshness gate — asserts the debug test binary was built AFTER the Rust
# sources currently checked out, not merely that it was rebuilt recently.
#
# WHY THIS EXISTS
# ---------------
# The standing rule all run was "rebuild and confirm the mtime advanced". On a
# box where `tests/e2e/harness/chiefd.ts` runs `cargo build --release` on EVERY
# suite process, that rule is satisfiable by a rebuild of UNCHANGED source: an
# e2e run in your own lane advances the mtime with no source change at all.
# Measured 2026-07-30: a binary built at 14:22:22 read 14:31:26 nine minutes
# later with nothing of the operator's changed in between.
#
# So an advancing mtime proves a REBUILD happened, not that the binary contains
# your change. It is necessary, not sufficient. The real invariant is three
# facts, not one:
#
#     1. the source changed        (it is in the checkout)
#     2. THEN it was built         (ordering)
#     3. and the mtime moved       (evidence the build half happened)
#
# This gate checks the ordering, which is the part a human eye skips. It is the
# difference between "I rebuilt" and "the thing I am about to test contains the
# fix I am testing" — and gating a Rust fix against a binary that predates it is
# the false-result class ticket T3 exists to prevent, arriving through a
# different door.
#
# USAGE
#   scripts/binary-freshness-gate.sh [path-to-binary]
# Exit 0 = binary postdates the newest committed Rust source. Exit 1 = STALE.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

target_root="${CARGO_TARGET_DIR:-apps/chiefd/target}/debug"
# The DEFAULT set is every binary the test gate consumes. `chiefd` joined it
# when P6 split the one executable that was operator client and backend at
# once: a gate that checked only `chief` would call a build fresh while the
# program that actually serves a company was months old.
binaries=("${1:-$target_root/chief}")
[[ $# -gt 0 ]] || binaries+=("$target_root/chiefd" "$target_root/beacond")

# The newest COMMIT that touched Rust sources. Committer date, because that is
# what moves on a rebase — an author date would let a rebased fix look older
# than a binary built before the rebase landed it.
source_commit_epoch="$(git log -1 --format=%ct -- 'apps/chiefd/**/*.rs' 2>/dev/null || true)"
if [[ -z "$source_commit_epoch" ]]; then
  echo "STALE-GATE: FAIL — no commit found touching apps/chiefd/**/*.rs" >&2
  echo "  This is a broken question, not a passing answer. Refusing to report green." >&2
  exit 1
fi

# Uncommitted Rust edits count too: their mtime is the real source timestamp.
# NOTE: deliberately NOT `while read ... < <(...)`. Process substitution is
# unavailable in some sandboxed shells here, and its failure made this script
# exit 1 for the WRONG REASON — which made a negative control look like it had
# passed. A control that returns the expected result for the wrong reason is
# worse than no control, so this uses a plain temp file.
# --- portability: mtimes and epoch formatting -------------------------------
#
# `stat -c` and `date -d` are GNU spellings that macOS REJECTS outright
# ("stat: illegal option -- c", "date: illegal option -- d"), so this gate
# aborted on every developer Mac before reaching a single assertion. Verified
# against real output on both: BSD wants `stat -f %m` and `date -r <epoch>`.
#
# Probed once, not per call, and the probe is the real command rather than a
# uname test: a uname branch encodes an assumption about which spelling a
# platform has, and this asks it.
if stat -c %Y . >/dev/null 2>&1; then
  _mtime() { stat -c %Y "$1"; }
else
  _mtime() { stat -f %m "$1"; }
fi
if date -d '@0' '+%F' >/dev/null 2>&1; then
  _epoch_local() { date -d "@$1" '+%F %T'; }
else
  _epoch_local() { date -r "$1" '+%F %T'; }
fi

_rs_list="$(mktemp)"
trap 'rm -f "$_rs_list"' EXIT
{ git diff --name-only HEAD -- 'apps/chiefd/**/*.rs' 2>/dev/null || true
  git ls-files --others --exclude-standard -- 'apps/chiefd/**/*.rs' 2>/dev/null || true
} > "$_rs_list"

newest_worktree_rs=0
while IFS= read -r f; do
  [[ -n "$f" && -f "$f" ]] || continue
  m="$(_mtime "$f")"
  (( m > newest_worktree_rs )) && newest_worktree_rs="$m"
done < "$_rs_list"

source_epoch="$source_commit_epoch"
(( newest_worktree_rs > source_epoch )) && source_epoch="$newest_worktree_rs"

printf 'source : %s  (%s)\n' "$source_epoch" "$(_epoch_local "$source_epoch")"
for binary in "${binaries[@]}"; do
  [[ -f "$binary" ]] || { echo "STALE-GATE: FAIL — no binary at $binary" >&2; exit 1; }
  binary_epoch="$(_mtime "$binary")"
  printf 'binary : %s  (%s)\n' "$binary_epoch" "$(_epoch_local "$binary_epoch")"
  (( binary_epoch > source_epoch )) || { echo "STALE-GATE: FAIL — $binary is stale" >&2; exit 1; }
done
echo "STALE-GATE: PASS — every checked binary postdates the newest Rust source"

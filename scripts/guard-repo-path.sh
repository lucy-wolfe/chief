#!/usr/bin/env bash
# guard-repo-path.sh — sourced by every merger tool that runs a state-moving
# git verb. Refuses any such verb resolving under the OPERATOR'S CHECKOUT.
#
# WHY THIS IS MECHANICAL RATHER THAN REMEMBERED
# ----------------------------------------------
# This is the third write to /root/workspace/chief. It followed a disclosure,
# a fleet-wide broadcast of the rule, and the building of /root/merger-canonical
# specifically to prevent it. Three statements of a rule that keeps being broken
# is the definition of a control that must be mechanical.
#
# The cause is structural, not carelessness: the merger's session STARTS in
# /root/workspace/chief. So "run it here" means "run it in the operator's
# checkout" -- the identical default that had three seats building on lucy.
# The default cannot be moved; the wrong thing can be made to refuse.
#
# SCOPE — WHAT THIS COVERS
#   Verbs:  rebase, reset, checkout, worktree add, merge, commit, push,
#           update-ref, branch -D/-f, clean, cherry-pick, am, apply, stash
#   Tools:  batch-merge.sh, land.sh, rebase-pin.sh — every tool the merger owns
#           that moves git state.
#
# SCOPE — WHAT THIS DOES NOT COVER, STATED PLAINLY
#   An ad-hoc `git` command typed directly into a tool call is NOT covered.
#   Nothing here reaches it. Both prior incidents were exactly that: a loop in
#   a tool call, not a line in a script. So the true statement is
#   "the merger's TOOLS cannot write to the operator's checkout", NOT
#   "the operator's checkout cannot be written to". The residual risk is a
#   human typing a git verb, and its enforcement remains discipline.
#
# WHY IT IS CHECKED IN
#   It lived in one seat's session scratchpad for a whole programme, which
#   meant it protected exactly the machines that seat had touched and vanished
#   with the session. A control that only exists in a scratchpad protects
#   nobody after the scratchpad is gone.

# The protected tree. GUARD_OPERATOR_CHECKOUT exists so the guard's own test
# can point it at a temporary directory: /root/workspace/chief does not exist
# on the build hosts or on CI, and a test that created it there would litter
# every machine it ran on and still not exercise the real comparison. It is a
# TEST SEAM, not a bypass -- production callers set nothing and get the real
# path. Anyone who sets it to disable the guard has chosen to, which this
# guard never claimed to prevent (see the scope statement above).
OPERATOR_CHECKOUT_REAL=$(readlink -f "${GUARD_OPERATOR_CHECKOUT:-/root/workspace/chief}" 2>/dev/null || echo "${GUARD_OPERATOR_CHECKOUT:-/root/workspace/chief}")

# assert_not_operator_checkout <dir> [verb]
#
# Resolves the REAL path (not the string): a relative path, a symlink, a failed
# `cd` leaving an unexpected cwd, or an unset variable expanding to empty must
# all land here rather than slip past a literal comparison. Both prior incidents
# were a path that was assumed rather than proven.
assert_not_operator_checkout() {
  local dir="${1-}" verb="${2:-git write}"

  # An empty/unset argument is the failure mode that caused incident two: a
  # failed `cd` left the shell somewhere unintended. Resolve to the CURRENT
  # directory in that case and check it, rather than passing silently.
  [ -n "$dir" ] || dir="$PWD"

  local real
  real=$(readlink -f "$dir" 2>/dev/null || true)
  # An unresolvable path is not proof of safety. Fail closed.
  [ -n "$real" ] || {
    echo "REFUSING ($verb): '$dir' does not resolve to a real path. An unresolvable path is not proof of safety."
    return 1
  }

  case "$real" in
    "$OPERATOR_CHECKOUT_REAL"|"$OPERATOR_CHECKOUT_REAL"/*)
      echo "REFUSING ($verb): '$real' is inside the OPERATOR'S CHECKOUT ($OPERATOR_CHECKOUT_REAL)."
      echo "  The merger never runs a state-moving git verb there. Use /root/merger-canonical"
      echo "  or a scratchpad worktree. Nothing was rebased, reset, checked out, or written."
      return 1 ;;
  esac
  return 0
}

# assert_git_toplevel_safe [verb]
#
# The stronger form: resolves the git TOPLEVEL of the current directory, so a
# subdirectory of the operator's checkout is caught as surely as its root.
assert_git_toplevel_safe() {
  local verb="${1:-git write}" top
  assert_not_operator_checkout "$PWD" "$verb" || return 1
  top=$(git rev-parse --show-toplevel 2>/dev/null || true)
  [ -n "$top" ] || return 0   # not in a repo at all — nothing to protect here
  assert_not_operator_checkout "$top" "$verb" || return 1
  return 0
}

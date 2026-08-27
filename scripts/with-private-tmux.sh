#!/usr/bin/env bash
# THE one wrapper that keeps a pre-push command off the operator's tmux server.
#
#   bash scripts/with-private-tmux.sh <command> [args...]
#
# # The defect this closes
#
# `bun run test:pre-push-guards` destroyed live tmux sessions belonging to
# several people on a shared box. #1205 contained the guard harness, which was
# correct and not enough: the same pre-push window also runs `cargo test`, and
# that is where the real-tmux tests live.
#
# # Two independent defences, and why not one
#
# 1. A private `TMUX_TMPDIR`. `tmux -L <name>` resolves to `$TMUX_TMPDIR`-or-
#    `/tmp`, plus `tmux-<uid>/`, plus the name — so the variable is the
#    NAMESPACE, not a scratch-file preference. With it set, even the literal
#    socket name `default` resolves to `<private>/tmux-<uid>/default`, a
#    different FILE from `/tmp/tmux-0/default`.
#
# 2. `TMUX` and `TMUX_PANE` are UNSET. This is the half #1205 did not have, and
#    it closes the path the widened hunt actually found:
#    `company.rs::boot_socket` has four tiers and TIER 3 IS THE AMBIENT `$TMUX`.
#    That variable is `<socket_path>,<pid>,<pane>`, and inside an operator's
#    pane its basename is literally `default`. `boot_socket_from_env` reads it
#    from the real environment, and eight product call sites go through it —
#    `attach`, `stop`, `listing`, `founder` (three) and `main` — several of
#    which then run destructive verbs.
#
#    So the dangerous socket name never had to appear in a test. It arrives
#    from whose terminal happened to launch the run. An audit of the socket
#    NAMES a fixture mints structurally cannot see that, which is why two
#    separate audits came back clean.
#
# Either defence alone would contain the failure. Both are here because this
# fault class has now escaped diagnosis twice — see `CHANGELOG.md`'s record of
# an earlier vanished-server incident that was never explained at all — and a
# containment that rests on one theory of a fault nobody has diagnosed is a
# containment that rests on a guess.
#
# # What it deliberately does NOT do
#
# It does not remove the namespace afterwards. Unlinking a socket file does not
# stop the server behind it, it makes it unreachable — cleanup would trade one
# visible empty directory for an invisible orphan process. One empty directory
# per run is the cheaper residue.
#
# It does not forbid `kill-server`. A run may destroy servers it created; the
# rule is that it must not be able to NAME one it did not.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: bash scripts/with-private-tmux.sh <command> [args...]" >&2
  exit 2
fi

# Forced, never defaulted. Honouring an ambient `TMUX_TMPDIR` would be safety
# that holds only while some unrelated setting happens to be right — and on a
# box where the operator runs inside tmux, the ambient value points AT the
# server being protected.
TMUX_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/chief-private-tmux-XXXXXX")"
chmod 700 "$TMUX_TMPDIR"
export TMUX_TMPDIR

# The pane identity of whoever launched this run is not a fact about the run.
unset TMUX
unset TMUX_PANE

exec "$@"

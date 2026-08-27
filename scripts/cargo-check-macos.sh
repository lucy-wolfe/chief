#!/usr/bin/env bash
# THE macOS cross-target check (AGENTS.md: every change must build on macOS
# and Linux; mode_t is u16 on Darwin and u32 on Linux). Owned by E1-S2 — do
# not hand-roll a second one, and do not add flags to skip it.
#
# `cargo check` never links, so this needs no Apple SDK: the only C
# compilation in the graph is rusqlite's `bundled` sqlite3.c, which is
# excluded for the Darwin target in apps/chiefd/Cargo.toml. Full osxcross was
# rejected (Apple SDK licensing) — the design record.
#
# Scope, stated plainly (#884): this is a COMPILE-ONLY cross-check. It proves
# the workspace type-checks and compiles for x86_64-apple-darwin from this
# Linux host; it never runs, links, or executes anything for that target and
# so cannot and does not verify macOS RUNTIME behavior. It is not a substitute
# for testing on real Darwin hardware if that ever becomes necessary.
#
# Warnings gate this check (#884): `-D warnings` turns any warning under the
# Darwin target — including target-specific code no native Linux check ever
# compiles — into a build failure. That is a deliberate, blanket choice, not
# an enumerated allowlist. A target-specific `#[cfg]` branch is exactly the
# code this second target exists to catch, so leaving it ungated defeats the
# point of running the check at all; an enumerated exception list would need
# a hand-maintained inventory of "known warnings," which is the exact defect
# class #907 names (a numeric/inventory guard that drifts because updating it
# looks identical to weakening it). `-D warnings` sidesteps that dilemma
# rather than picking a category from #907's taxonomy: it is not a vacuity
# floor, a detection threshold, or a loss ratchet, because it carries no
# number and no list to go stale — the bound is "zero," which needs no
# maintenance and cannot silently widen. #884 fixed the two warnings that had
# already accumulated before adding this gate, precisely because nothing was
# enforcing it before.
#
# Delivered via RUSTFLAGS, not `cargo check -- -D warnings` (#884): unlike
# `cargo clippy`, which documents `-- -D warnings` as its lint-level idiom,
# plain `cargo check` does not forward trailing args to rustc for a
# multi-target `--workspace --all-targets` invocation — it rejects them as
# unrecognized cargo flags. `RUSTFLAGS="-D warnings"` is the standard
# workaround and is safe for a workspace-wide gate: cargo automatically adds
# `--cap-lints allow` for path/registry dependencies outside this workspace,
# so this only denies warnings in the workspace's own crates, never in
# upstream dependency code we do not control.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="x86_64-apple-darwin"
MANIFEST="$ROOT/apps/chiefd/Cargo.toml"

# Informational only — not a gate, not a loss ratchet: counts the workspace's
# `members = [...]` entries in the manifest so a clean run states what it
# actually covered instead of leaving the scope to be inferred.
CRATE_COUNT="$(awk '/^members = \[/{flag=1; next} /^\]/{flag=0} flag' "$MANIFEST" | grep -c '^[[:space:]]*"')"

rustup target add "$TARGET"

echo "cargo-check-macos: target=$TARGET manifest=apps/chiefd/Cargo.toml workspace_crates=$CRATE_COUNT warnings=gated(RUSTFLAGS=-D warnings) scope=compile-only(no link/run)"
RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --locked --target "$TARGET" \
  --manifest-path "$MANIFEST"
echo "cargo-check-macos: PASS target=$TARGET workspace_crates=$CRATE_COUNT warnings=0"

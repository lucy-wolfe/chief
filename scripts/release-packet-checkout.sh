#!/usr/bin/env bash
# #1004: releasing a packet checkout AFTER its build artifacts have entered
# the shared, persistent CARGO_TARGET_DIR (see gate-matrix.sh) leaves that
# dir holding compiled artifacts whose baked absolute paths
# (`env!("CARGO_MANIFEST_DIR")`, #1002) no longer resolve once the checkout
# is gone. Measured on a build host after `/root/b68` was released: 648
# baked references to `/root/b68/repo` across libbeacond, libchiefd_api,
# libchiefd_core and libchiefd_host.
#
# THE FIX IS AT RELEASE TIME, NOT AS A STRONGER PRE-GATE CHECK
# ---------------------------------------------------------------
# "A check is a property of a moment, not a property of the host." A
# pre-gate scan for stale references can only be honest about the moment it
# ran -- it cannot see a release that has not happened yet, and re-running
# a stronger version of the same scan before the NEXT gate only ever
# reports the PREVIOUS release's damage after the fact. The invariant has
# to be enforced by the action that creates the condition: releasing a
# packet's crates cleans their build artifacts out of the shared dir as
# part of the same operation, so no later reader ever has to know which
# directories were released or when.
#
# Usage:
#   scripts/release-packet-checkout.sh <checkout-path> <crate>...
#
# Example (the exact remediation used on zipbox for #1004):
#   scripts/release-packet-checkout.sh /root/b68 beacond chiefd-api chiefd-core chiefd-host chiefd
#
# This does NOT remove the checkout directory itself -- that is the
# caller's own worktree/directory teardown, orthogonal to this script.
# This only cleans the shared target dir of the released crates' artifacts
# and PROVES the clean took, rather than reporting that it ran.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <checkout-path> <crate>..." >&2
  exit 1
fi

CHECKOUT_PATH="$1"
shift
CRATES=("$@")

: "${CARGO_TARGET_DIR:=/root/cargo-targets-shared}"
export CARGO_TARGET_DIR

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/apps/chiefd/Cargo.toml"

# Normalize: strip a trailing slash so the strings-scan substring match
# below cannot miss a path differing only in trailing-slash presence.
CHECKOUT_PATH="${CHECKOUT_PATH%/}"

echo "== release-packet-checkout: cleaning ${#CRATES[@]} crate(s) for $CHECKOUT_PATH from $CARGO_TARGET_DIR =="
cargo clean --manifest-path "$MANIFEST" -p "${CRATES[@]}"

# Verifying a clean rather than reporting one (#1004): a text grep over the
# tree is the WRONG instrument here -- compiled/linked artifacts are
# binary, and a `grep -r` can return clean while the baked path is still
# present. `strings -a` is what actually finds it, exactly as the manual
# remediation on zipbox did.
if [ ! -d "$CARGO_TARGET_DIR" ]; then
  echo "== release-packet-checkout: $CARGO_TARGET_DIR does not exist -- nothing to verify =="
  exit 0
fi

remaining="$(find "$CARGO_TARGET_DIR" -type f 2>/dev/null | xargs -r strings -a 2>/dev/null | grep -Fc "$CHECKOUT_PATH" || true)"

if [ "${remaining:-0}" -ne 0 ]; then
  echo "REFUSING: $remaining reference(s) to $CHECKOUT_PATH remain in $CARGO_TARGET_DIR after"
  echo "  cargo clean -p ${CRATES[*]}. Either the crate list is incomplete (another crate also"
  echo "  baked this path) or a non-crate artifact (e.g. a raw build script output) carries it."
  echo "  Re-scan with: find \"$CARGO_TARGET_DIR\" -type f | xargs strings -a | grep -F \"$CHECKOUT_PATH\""
  exit 1
fi

echo "== release-packet-checkout: verified -- zero references to $CHECKOUT_PATH remain =="

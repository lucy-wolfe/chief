#!/usr/bin/env bash
# Sweep stale test scratch directories out of /tmp.
#
# WHY THIS EXISTS (#84): /tmp is a 7.9G tmpfs shared by every agent on the box,
# and it is finite in INODES as well as bytes. Test suites that leave temp dirs
# behind accumulate until tmpfs exhaustion starts failing UNRELATED suites —
# which is the expensive part, because the resulting red has nothing to do with
# the code under test and sends people debugging the wrong thing. A cleanup pass
# once turned 76 failures into 277 pass / 0 fail.
#
# The permanent fix is that suites clean up after themselves (see the #84
# changes to tests/triber-chiefd-process.test.ts and tests/setup-durable-store.ts).
# This script is for the historical strays those fixes cannot retroactively
# remove, and as a periodic safety net for any leak not yet found.
#
# SAFETY: only removes directories matching a KNOWN test prefix, and only ones
# older than AGE_MINUTES (default 120) so a running suite is never touched.
# Prints what it removes; --dry-run prints without removing.
set -euo pipefail

AGE_MINUTES="${AGE_MINUTES:-120}"
DRY_RUN=""
[[ "${1:-}" == "--dry-run" ]] && DRY_RUN=1

# Prefixes owned by this repo's suites. Deliberately explicit: a glob like
# /tmp/*-* would eventually delete somebody else's working state.
PREFIXES=(
  chiefd-process-test
  suite-unconfigured-pi-home
  chiefd-docstore-test
  organization-intercom
  organization-intercom-fleet-gate
  organization-assignment-cli
  store-lock-retry
  launcher-materialize
  e2e-chiefd
  e2e-org-world-data
  pi-native-session-e2e
  organization-footer-mailbox
  organization-footer-changed-store
  organization-footer-missed-event
  organization-footer-goals
)

total=0
for prefix in "${PREFIXES[@]}"; do
  # A plain pipeline rather than process substitution: `/dev/fd` is not always
  # available in the sandboxes this runs in, and `total` is recomputed after the
  # loop so the subshell's lost increments do not matter.
  found=$(find /tmp -maxdepth 1 -type d -name "${prefix}-*" -mmin "+${AGE_MINUTES}" 2>/dev/null || true)
  [[ -z "$found" ]] && continue
  while IFS= read -r dir; do
    [[ -z "$dir" ]] && continue
    total=$((total + 1))
    if [[ -n "$DRY_RUN" ]]; then
      echo "would remove $dir"
    else
      rm -rf -- "$dir"
      echo "removed $dir"
    fi
  done <<< "$found"
done

echo "${DRY_RUN:+[dry-run] }swept ${total} stale test directories (older than ${AGE_MINUTES}m)"

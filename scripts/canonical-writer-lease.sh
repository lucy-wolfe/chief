#!/usr/bin/env bash
# canonical-writer-lease.sh — make the sole-canonical-writer rule enforced
# rather than merely observed.
#
# WHY
# ---
# On 2026-08-07 two merger seats were briefly alive at once. The original went
# unresponsive at ~07:05 and was replaced; at 09:40 — two and a half hours and
# fourteen batches later — it woke up and immediately acted: re-provisioned its
# checkout, cut a `batch42 candidate` commit, and launched a gate run against
# the shared CARGO_TARGET_DIR on the host where the live merger's batch 56 gate
# was mid-flight. All of it BEFORE acknowledging the liveness check that woke
# it.
#
# It could not push, BY ACCIDENT RATHER THAN BY DESIGN: its checkout had no
# remote configured. Nothing prevented it from adding one.
#
# `scripts/guard-repo-path.sh` refuses state-moving git verbs against the
# operator's checkout. There has been no equivalent protecting
# origin/revamp/monorepo from a second writer. The rule held that night because
# one seat happened to be unresponsive during the window — that is luck, not
# enforcement. A rule that has never been violated is indistinguishable from a
# rule that cannot be violated, until the day two actors are live at once.
#
# THE SPECIFIC HAZARD IS A WAKE-UP ACTION BEFORE ORIENTATION. The stale seat's
# first act was to commit and launch a build, not to ask where things stood. So
# this refuses the FIRST write from an unleased seat rather than warning after
# it.
#
# WHY A REF AND NOT A FILE
# ------------------------
# The lease lives at `refs/leases/canonical` ON THE REMOTE, because that is the
# only thing both seats can see. A file in a working copy is invisible to the
# other actor by construction — which is exactly how the original incident
# stayed undetected until someone read the process list.
#
# The claim is a COMPARE-AND-SWAP: `git push --force-with-lease=<ref>:<observed>`
# fails if anyone moved the ref between the read and the write. Two seats racing
# cannot both win. Without CAS this would be a check-then-act with a window, and
# a lease with a race in its own acquisition is theatre.
#
# COMMITTER IDENTITY IS A SEPARATE GAP AND THIS DOES NOT CLOSE IT. Every commit
# both mergers produce is the same name and email, so auditing canonical for
# foreign writes BY AUTHOR is impossible. What this adds instead is `--audit`,
# which checks the structural invariant that made the original audit possible at
# all: every commit on canonical in a range is a numbered batch, the numbers are
# monotonic, and each has a retained gate ref. That was true by coincidence of
# convention; nothing verified it.
#
# USAGE
#   canonical-writer-lease.sh --claim   [--seat NAME] [--ttl SECONDS]
#   canonical-writer-lease.sh --verify  [--seat NAME] [--ttl SECONDS]
#   canonical-writer-lease.sh --release [--seat NAME]
#   canonical-writer-lease.sh --show
#   canonical-writer-lease.sh --audit RANGE      # e.g. --audit a274b743f..HEAD
#
# --seat defaults to $CANONICAL_SEAT, then to "$(hostname)/$$".
# --ttl defaults to 3600s. A lease older than its TTL is STALE and reclaimable:
# a seat that dies holding one must not block the fleet forever.
#
# EXIT 0 = you hold it / audit clean. EXIT 1 = refused or audit failed.
# FAILS CLOSED: any state this cannot establish is a refusal, never a pass.

set -euo pipefail

LEASE_REF="refs/leases/canonical"
REMOTE="${CANONICAL_REMOTE:-origin}"
SEAT="${CANONICAL_SEAT:-$(hostname)/$$}"
TTL=3600
MODE=""
RANGE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --claim|--verify|--release|--show) MODE="${1#--}"; shift ;;
    --audit) MODE="audit"; RANGE="$2"; shift 2 ;;
    --seat)  SEAT="$2"; shift 2 ;;
    --ttl)   TTL="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$MODE" ] || { echo "one of --claim/--verify/--release/--show/--audit is required" >&2; exit 2; }

now() { date -u +%s; }

# Reads the lease FROM THE REMOTE every time. Never from a local cache: a stale
# fetch reporting "no lease" is precisely the false negative that lets a second
# writer through, and it is the same mechanism that made a pushed branch look
# absent earlier in this programme.
read_lease() {
  local sha
  sha="$(git ls-remote "$REMOTE" "$LEASE_REF" 2>/dev/null | awk '{print $1}')" || return 1
  printf '%s' "$sha"
}

lease_body() { git cat-file -p "$1" 2>/dev/null || true; }

field() { printf '%s\n' "$2" | sed -n "s/^$1=//p" | head -1; }

case "$MODE" in
  show)
    sha="$(read_lease || true)"
    if [ -z "$sha" ]; then echo "no lease held on $REMOTE $LEASE_REF"; exit 0; fi
    git fetch -q "$REMOTE" "$LEASE_REF" 2>/dev/null || true
    body="$(lease_body "$sha")"
    echo "lease $sha"
    printf '%s\n' "$body" | sed 's/^/  /'
    ;;

  claim)
    observed="$(read_lease || true)"
    if [ -n "$observed" ]; then
      git fetch -q "$REMOTE" "$LEASE_REF" 2>/dev/null || true
      body="$(lease_body "$observed")"
      if [ -z "$body" ]; then
        echo "REFUSED: $LEASE_REF exists at $observed but its object could not be read." >&2
        echo "  Cannot establish who holds it. Fails closed." >&2
        exit 1
      fi
      holder="$(field seat "$body")"
      epoch="$(field epoch "$body")"
      age=$(( $(now) - ${epoch:-0} ))
      if [ "$holder" != "$SEAT" ] && [ "$age" -lt "$TTL" ]; then
        echo "REFUSED: canonical is leased by \"$holder\", claimed ${age}s ago (TTL ${TTL}s)." >&2
        echo "  You are \"$SEAT\". Do not write to canonical." >&2
        echo "  If that seat is genuinely dead, wait out the TTL or release it deliberately." >&2
        exit 1
      fi
      [ "$holder" != "$SEAT" ] && echo "note: reclaiming a STALE lease from \"$holder\" (${age}s > TTL ${TTL}s)"
    fi

    blob="$(printf 'seat=%s\nepoch=%s\nhost=%s\n' "$SEAT" "$(now)" "$(hostname)" | git hash-object -w --stdin)"
    # Compare-and-swap against exactly what was observed above. If another seat
    # moved the ref in between, this push fails and so do we -- which is the
    # entire point.
    if [ -n "$observed" ]; then
      git push -q "$REMOTE" "$blob:$LEASE_REF" --force-with-lease="$LEASE_REF:$observed" 2>/dev/null || {
        echo "REFUSED: the lease moved between read and write -- another seat claimed it." >&2
        exit 1
      }
    else
      git push -q "$REMOTE" "$blob:$LEASE_REF" 2>/dev/null || {
        echo "REFUSED: could not create $LEASE_REF (another seat may have created it first)." >&2
        exit 1
      }
    fi
    echo "lease held by \"$SEAT\" ($blob)"
    ;;

  verify)
    observed="$(read_lease || true)"
    [ -n "$observed" ] || { echo "REFUSED: no lease held. Claim it before writing to canonical." >&2; exit 1; }
    git fetch -q "$REMOTE" "$LEASE_REF" 2>/dev/null || true
    body="$(lease_body "$observed")"
    holder="$(field seat "$body")"
    epoch="$(field epoch "$body")"
    age=$(( $(now) - ${epoch:-0} ))
    if [ "$holder" != "$SEAT" ]; then
      echo "REFUSED: lease is held by \"$holder\", not \"$SEAT\"." >&2; exit 1
    fi
    if [ "$age" -ge "$TTL" ]; then
      echo "REFUSED: your own lease has expired (${age}s >= TTL ${TTL}s). Re-claim before writing." >&2; exit 1
    fi
    echo "lease verified for \"$SEAT\" (${age}s old)"
    ;;

  release)
    observed="$(read_lease || true)"
    [ -n "$observed" ] || { echo "no lease to release"; exit 0; }
    git fetch -q "$REMOTE" "$LEASE_REF" 2>/dev/null || true
    holder="$(field seat "$(lease_body "$observed")")"
    if [ "$holder" != "$SEAT" ]; then
      echo "REFUSED: refusing to release a lease held by \"$holder\" (you are \"$SEAT\")." >&2; exit 1
    fi
    git push -q "$REMOTE" --delete "$LEASE_REF" 2>/dev/null || {
      echo "REFUSED: could not delete $LEASE_REF." >&2; exit 1; }
    echo "lease released by \"$SEAT\""
    ;;

  audit)
    # The structural invariant, asserted rather than assumed. It was already
    # true; nothing verified it, so it held by coincidence of convention.
    fail=0
    prev=""
    while read -r sha subject; do
      [ -n "$sha" ] || continue
      if ! printf '%s' "$subject" | grep -qE '^batch [0-9]+:'; then
        echo "FOREIGN WRITE: $sha $subject" >&2
        echo "  not a numbered batch commit -- there is room in the sequence for a write nobody accounted for" >&2
        fail=1
        continue
      fi
      n="$(printf '%s' "$subject" | sed -n 's/^batch \([0-9]*\):.*/\1/p')"
      if [ -n "$prev" ] && [ "$n" -ge "$prev" ]; then
        echo "NON-MONOTONIC: batch $n appears after batch $prev ($sha)" >&2
        fail=1
      fi
      prev="$n"
      if ! git ls-remote "$REMOTE" "refs/gates/batch-$n" 2>/dev/null | grep -q .; then
        echo "NO RETAINED GATE REF: batch $n ($sha) has no refs/gates/batch-$n on $REMOTE" >&2
        echo "  its gated tree is unrecoverable, so the landing claim cannot be checked (#985)" >&2
        fail=1
      fi
      if [ "$(git rev-list --count "$sha^@" 2>/dev/null)" != "" ] && [ "$(git cat-file -p "$sha" | grep -c '^parent ')" -gt 1 ]; then
        echo "MERGE COMMIT: $sha $subject -- canonical is linearized; a merge is a foreign shape" >&2
        fail=1
      fi
    done < <(git log --format='%H %s' "$RANGE" 2>/dev/null)
    if [ "$fail" = "0" ]; then
      echo "audit clean over $RANGE: every commit is a numbered batch, numbers descend monotonically, every batch has a retained gate ref, no merges"
    fi
    exit "$fail"
    ;;
esac

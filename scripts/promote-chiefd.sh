#!/usr/bin/env bash
#
# Promote a freshly built chiefd binary and restart the daemon — safely.
#
# WHY THIS SCRIPT EXISTS (#112, 2026-07-24)
#
# A deploy to the live trading company promoted with a bare `cp` over the
# RUNNING binary. Linux refuses that with ETXTBSY, `cp` printed one line and
# returned non-zero, and the surrounding commands carried on and restarted the
# daemon anyway — on the OLD binary. Every downstream signal said success:
#
#     new listener pid
#     tmux socket resolved ... provenance="explicit"
#     startup self-audit complete ... raised=3
#     duty scheduler started ... mode=Apply docstore_mounted=true
#     store surface listening ... db=<derived company database>
#     single instance, 0 zombies, 0 shadow panes, 0 dead panes
#
# ALL TRUE. ALL IRRELEVANT. The standing boot gate verifies that the daemon
# booted CORRECTLY; it cannot see whether the daemon is NEW. That is a complete,
# well-formed verification of the wrong proposition, and only an artifact check
# caught it.
#
# The three rules below are the fix. They are a script rather than a runbook
# paragraph on purpose: a rule that depends on somebody remembering it is not a
# control. This must survive the operator being tired, interrupted, or replaced.
#
#   1. rename(2), not cp — atomic, and NOT blocked by ETXTBSY, because it
#      replaces the directory entry rather than writing through to the inode the
#      kernel has mapped. Guarded by a same-filesystem check: rename ACROSS
#      devices fails, and a fallback to copy semantics would reintroduce the
#      original defect while wearing the new fix's name.
#   2. Gate the restart on the promote's exit code. The whole incident is that a
#      failed promote was followed by a successful-looking restart.
#   3. Assert the ARTIFACT, not just the boot. Primary assertion is
#      /proc/<pid>/exe compared byte-for-byte against the build output — the only
#      probe that reads what the kernel actually mapped rather than what a path
#      claims. Marker strings are secondary, and every marker needs a
#      DIFFERENTIAL against the outgoing binary: a marker present in both proves
#      nothing. Assert on string LITERALS only; release builds do not retain
#      function names (measured: `supervision_cycle` -> 0 hits, "supervision
#      cycle committed" -> 1).
#
# Usage:
#   promote-chiefd.sh --built <path> --target <path> --start <path> --dir <company directory> [--marker <literal>]...
#
# Every marker must be ABSENT from the outgoing binary and PRESENT in the new
# one. Pass one per commit whose landing you are verifying.

set -o pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib/company-process-count.sh
source "$HERE/lib/company-process-count.sh"

die() { printf '\n\033[31mDEPLOY ABORTED:\033[0m %s\n' "$*" >&2; exit 1; }
say() { printf '  %s\n' "$*"; }

BUILT="" TARGET="" START="" COMPANY_DIR="" CHECK_ONLY=0 ; MARKERS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --built)  BUILT="$2";  shift 2 ;;
    --target) TARGET="$2"; shift 2 ;;
    --start)  START="$2";  shift 2 ;;
    --marker) MARKERS+=("$2"); shift 2 ;;
    # THE company. `--company <slug>` named a display word that two directories
    # may share; the directory is the identity, and it is the one thing
    # `chiefd run` is told.
    --dir) COMPANY_DIR="$2"; shift 2 ;;
    # Run the ARTIFACT checks and stop, touching nothing. Lets you validate a
    # marker set before committing to a restart — and lets the guards be tested
    # without a live daemon.
    --check-only) CHECK_ONLY=1; shift ;;
    *) die "unknown argument '$1'" ;;
  esac
done
[ -n "$BUILT" ] && [ -n "$TARGET" ] && [ -n "$START" ] || die "--built, --target and --start are all required"
[ -x "$BUILT" ] || die "built binary is missing or not executable: $BUILT"
[ -x "$START" ] || die "start script is missing or not executable: $START"

# Beacond's company registration is the authority for the selected daemon.
# Its argv-scoped `pgrep` fallback covers a temporary registration gap without
# broadening the search to another company. #52: a leaked zombie made chiefd's
# real pid unfindable, a restart killed a corpse, and two daemons ran
# concurrently against one company. A socket cannot be held by a zombie, so
# neither source may silently select an ambiguous process.
BEACOND_URL="${BEACOND_URL:-http://127.0.0.1:8790}"
BEACOND_BUILT="$(dirname "$BUILT")/beacond"
BEACOND_TARGET="$(dirname "$TARGET")/beacond"
company_field() {
  curl --fail --silent --max-time 2 "$BEACOND_URL/v1/lookup?dir=$COMPANY_DIR" \
    | python3 -c "import json,sys; print((json.load(sys.stdin).get('company') or {}).get('$1') or '')"
}
listener_pid() {
  local pid
  pid="$(company_field pid 2>/dev/null || true)"
  # `([[:space:]]|$)` anchors the argv word: `pgrep -f` matches an ERE, and
  # `--dir …/companies/acme` is a PREFIX of `--dir …/companies/acme-corp`, so
  # without it this fallback can hand a SIBLING company's pid to `kill -TERM`.
  [ -n "$pid" ] || pid="$(pgrep -f "$(basename "$TARGET") run --dir $COMPANY_DIR"'([[:space:]]|$)' | head -1 || true)"
  printf '%s' "$pid"
}

echo "== 1. stage on the target's filesystem =="
STAGED="${TARGET}.new"
BEACOND_STAGED="${BEACOND_TARGET}.new"
BUILT_FS=$(df -P "$BUILT" | awk 'NR==2 {print $1}')
TARGET_FS=$(df -P "$(dirname "$TARGET")" | awk 'NR==2 {print $1}')
say "built  fs: $BUILT_FS"
say "target fs: $TARGET_FS"
cp "$BUILT" "$STAGED" || die "could not stage the new binary at $STAGED"
cp "$BEACOND_BUILT" "$BEACOND_STAGED" || die "could not stage beacond at $BEACOND_STAGED"
STAGED_FS=$(df -P "$STAGED" | awk 'NR==2 {print $1}')
[ "$STAGED_FS" = "$TARGET_FS" ] \
  || die "staged file is on $STAGED_FS but the target is on $TARGET_FS — rename(2) cannot cross devices, and falling back to copy semantics would reintroduce the ETXTBSY bug this script exists to prevent"
# `wc -c <file` is POSIX and needs no GNU/BSD branch, unlike `stat -c %s`
# (rejected by macOS) or `stat -f %z` (rejected by GNU).
say "staged $(wc -c < "$STAGED" | tr -d " ") bytes"

echo "== 2. artifact check on the STAGED file, with a differential =="
[ "${#MARKERS[@]}" -gt 0 ] || say "(no --marker given: skipping the differential — you are trusting the build)"
for marker in "${MARKERS[@]}"; do
  # `grep -c` PRINTS 0 and EXITS 1 when there are no matches, so the obvious
  # `|| echo 0` appends a SECOND zero and yields the two-line string "0\n0",
  # which fails `[ "$old_hits" -eq 0 ]` and aborts every deploy — a guard that
  # refuses everything looks identical to a guard that works, because the
  # refusal tests still pass. `|| true` swallows the status without adding
  # output. (Caught by the positive-half test, not by the five refusal tests.)
  new_hits=$( { strings "$STAGED" || true; } | grep -c -- "$marker" || true )
  old_hits=$( { strings "$TARGET" 2>/dev/null || true; } | grep -c -- "$marker" || true )
  say "marker '$marker': staged=$new_hits outgoing=$old_hits"
  [ "$new_hits" -ge 1 ] || die "marker '$marker' is ABSENT from the newly built binary — the build did not include the change you are deploying"
  [ "$old_hits" -eq 0 ] || die "marker '$marker' is ALREADY in the outgoing binary — it proves nothing, so it is not a usable differential. Pick a literal introduced by this change."
done
# Positive control: a literal that has been in every build for months. If this
# is missing, `strings` itself is not answering the question we think it is.
CONTROL="supervision cycle committed"
[ "$( { strings "$STAGED" || true; } | grep -c -- "$CONTROL" || true )" -ge 1 ] \
  || die "positive control '$CONTROL' not found in the staged binary — the probe is invalid, so its other answers mean nothing"
say "positive control present"

if [ "$CHECK_ONLY" = "1" ]; then
  rm -f "$STAGED" "$BEACOND_STAGED"
  printf '\n\033[32mARTIFACT CHECKS PASSED\033[0m (--check-only: nothing was promoted)\n'
  exit 0
fi

[ -n "$COMPANY_DIR" ] || die "--dir <company directory> is required unless --check-only is used"

# A bare manual `chiefd run` defaults to duties-only: it never mounts the
# typed docstore and therefore cannot see the normalized company rows. This
# generic promote helper is still a ChiefD deploy surface, so it owns the
# store binding instead of trusting whatever a login shell happened to export.
# There is nothing to override any more: the store is the company directory's
# own file, so a second database would have to be a second company. An
# artifact-only `--check-only` needs no daemon/store and returns above before
# this launch-specific gate.
COMPANY_DB="$COMPANY_DIR/.chief/db/chief.db"
[ -f "$COMPANY_DB" ] || die "canonical company database is missing: $COMPANY_DB"
say "canonical company database: $COMPANY_DB"

echo "== 3. promote via rename(2) =="
OLD_PID=$(listener_pid)
say "current listener pid: ${OLD_PID:-<none>}"
mv -f "$STAGED" "$TARGET" || die "PROMOTE FAILED — refusing to restart. The daemon is untouched and still serving the old binary."
mv -f "$BEACOND_STAGED" "$BEACOND_TARGET" || die "BEACOND PROMOTE FAILED — refusing to restart chiefd"
say "promoted $(wc -c < "$TARGET" | tr -d " ") bytes"

echo "== 4. stop the old daemon =="
if [ -n "$OLD_PID" ]; then
  kill -TERM "$OLD_PID" 2>/dev/null
  for _ in $(seq 1 20); do kill -0 "$OLD_PID" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$OLD_PID" 2>/dev/null; then
    # #47: MEASURED IN PRODUCTION on 2026-07-24 — chiefd did not stop on SIGTERM
    # within 10s on three consecutive restarts. Escalating is correct here, but
    # it is also a defect worth fixing rather than a normal outcome.
    say "SIGTERM did not stop pid $OLD_PID within 10s — escalating to SIGKILL (#47)"
    kill -9 "$OLD_PID" 2>/dev/null; sleep 1
  else
    say "exited on SIGTERM"
  fi
fi
[ -z "$(listener_pid)" ] || die "beacond still reports a live daemon after the stop"

echo "== 5. start =="
BEACOND_PID=$(pgrep -f "^$BEACOND_TARGET" | head -1 || true)
if [ -n "$BEACOND_PID" ]; then
  kill -TERM "$BEACOND_PID" 2>/dev/null || true
  for _ in $(seq 1 20); do kill -0 "$BEACOND_PID" 2>/dev/null || break; sleep 0.25; done
  if kill -0 "$BEACOND_PID" 2>/dev/null; then
    kill -KILL "$BEACOND_PID" 2>/dev/null || true
    for _ in $(seq 1 20); do kill -0 "$BEACOND_PID" 2>/dev/null || break; sleep 0.25; done
  fi
  kill -0 "$BEACOND_PID" 2>/dev/null \
    && die "old beacond pid $BEACOND_PID survived SIGTERM and SIGKILL"
fi
# `~/.chief`, the box's own chief directory — `beacond::config`'s documented
# default and where `release-chiefd.ts` installs. `~/.chiefd` was the retired
# global company tree, and starting beacond against it would mint an empty
# registry beside the real one.
: "${HOME:?HOME is required to resolve the beacond registry}"
setsid nohup env BEACOND_DB_PATH="$HOME/.chief/beacond.sqlite" BEACOND_BIND="127.0.0.1:8790" "$BEACOND_TARGET" >>/dev/null 2>&1 </dev/null &
for _ in $(seq 1 20); do curl --fail --silent --max-time 1 "$BEACOND_URL/v1/health" >/dev/null 2>&1 && break; sleep 0.25; done
curl --fail --silent --max-time 1 "$BEACOND_URL/v1/health" >/dev/null || die "promoted beacond did not become healthy"
setsid nohup env BEACOND_URL="$BEACOND_URL" "$START" >>/dev/null 2>&1 </dev/null &
for _ in $(seq 1 30); do [ -n "$(listener_pid)" ] && break; sleep 0.5; done
NEW_PID=$(listener_pid)
[ -n "$NEW_PID" ] || die "beacond has no location for '$COMPANY_DIR' after 15s — the daemon did not come up"
say "new listener pid: $NEW_PID"

echo "== 6. PRIMARY ASSERTION: the running image is the binary we built =="
# /proc/<pid>/exe is the inode the kernel mapped. It cannot be satisfied by a
# path that was swapped, reverted, or never written — which is exactly how the
# ETXTBSY no-op passed every other check.
cmp -s "/proc/$NEW_PID/exe" "$BUILT" \
  || die "the RUNNING IMAGE differs from the build output. The daemon is up but it is NOT running the code you built."
say "/proc/$NEW_PID/exe == $BUILT (byte-identical)"

echo "== 7. single instance =="
INSTANCES="$(company_chiefd_instance_count "$TARGET" "$COMPANY_DIR")"
[ "$INSTANCES" -eq 1 ] || die "$INSTANCES chiefd instances serve '$COMPANY_DIR' — expected exactly 1 (#52)"
say "exactly 1 instance"

printf '\n\033[32mPROMOTED AND VERIFIED\033[0m  pid=%s\n' "$NEW_PID"
echo "Still to check by hand: the boot lines (provenance, docstore_mounted, db path),"
echo "the wedge sweep across both tmux sockets, and whatever this deploy's"
echo "behavioural expectation was. This script proves the binary is NEW and"
echo "SINGLE; it does not prove the change did what it claims."

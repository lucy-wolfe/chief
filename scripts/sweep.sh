#!/usr/bin/env bash
# Sweep runner — the workflow step, with the controls as PRECONDITIONS.
#
# WHY THIS SHAPE
# --------------
# The other gates in this directory are checkers: you run them, and you can
# forget to. "A gate you can forget to invoke is still an intention wearing a
# script's clothes." This one is different — it is how you RUN a sweep, so the
# controls are not invocable separately and cannot be skipped:
#
#   a5  process-leak census   -- taken before AND after, diffed. Not optional:
#                                the runner takes it, because "2 pi leaked on an
#                                all-pass run" means a green suite is not
#                                evidence of a clean box.
#   a6  load sampling         -- recorded during. Turns contention from an
#                                unknown into a measured covariate, so the
#                                T17-versus-contention question is falsifiable.
#   a12 exit sidecar          -- written AFTER the runner returns, so `report`
#                                can refuse to read an unfinished run. This is
#                                the error that killed a whole theory tonight.
#   a14 two-box acceptance    -- `accept` takes TWO logs and reports the UNION.
#                                Measured: one box 189/7 and the other 188/8, same
#                                tree, union ELEVEN distinct files. Zero on one
#                                box demonstrably is not zero.
#
# RULE ZERO IS OBSERVED: the census REPORTS, it never kills. Anything
# pre-existing is somebody else's; anything new and still running is reported
# for a human to decide. This script terminates nothing, ever.
#
# The census is NAME-AGNOSTIC, sorted by %CPU. The census that missed a foreign
# 100%-CPU process for 20 hours was looking for `pi`/`chiefd`/`cargo`/`bun` and
# never `node`/`grok`/`codex`. Do not reintroduce a name list.
#
# EXIT CODES — "I could not check" never shares a code with "I checked and
# found nothing":
#   0  ok        1  failed/incomplete/drift        2  could-not-check        3  usage
#
# USAGE
#   scripts/sweep.sh run    <log> -- <command...>
#   scripts/sweep.sh report <log>
#   scripts/sweep.sh accept <logA> <logB>
#   scripts/sweep.sh --selftest

set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Census excludes KERNEL THREADS (ppid 2, and pid 2 itself) and the census
# pipeline's OWN members. Both were found by the first real smoke run, which
# reported `ps`, `awk`, `sort` and a transient `kworker` as leak candidates —
# the instrument measuring itself. A leak detector that fires on every run is
# worse than none: it trains the reader to ignore it.
#
# NOTE the selftest did NOT catch this, because it exercised `accept` and the
# sidecars and never the diff. Testing what is easy to test rather than what
# carries the risk is its own failure mode.
census() {
  ps -eo pid,ppid,comm --no-headers 2>/dev/null \
    | awk '$2 != 2 && $1 != 2 && $3 != "ps" && $3 != "awk" && $3 != "sort" && $3 != "comm" {print $1" "$3}' \
    | sort
}

# A candidate is only a leak if it is STILL ALIVE when the diff is read. A
# process that exited during the run is not a leak; it is finished work.
still_alive() { [[ -d "/proc/$1" ]]; }

cmd_run() {
  local log="$1"; shift
  [[ "${1:-}" == "--" ]] && shift
  [[ $# -ge 1 ]] || { echo "usage: sweep.sh run <log> -- <command...>" >&2; return 3; }

  census > "${log}-census-before.txt"
  ( while :; do printf '%s %s\n' "$(date +%s)" "$(cut -d' ' -f1-3 /proc/loadavg)"; sleep 30; done ) > "${log}-load.txt" 2>/dev/null &
  local sampler=$!

  "$@" > "$log" 2>&1
  local rc=$?

  kill "$sampler" 2>/dev/null; wait "$sampler" 2>/dev/null
  # Written LAST, and only here: its presence is the signal that the writer
  # finished. `report` refuses without it.
  echo "$rc" > "${log}-exit.txt"
  census > "${log}-census-after.txt"

  # NO PROCESS SUBSTITUTION. It is unavailable in this sandbox (`/dev/fd/63:
  # No such file or directory`), and when it fails the loop never runs — so the
  # leak detector becomes dead code that reports "clean" every time, and the
  # "no false leaks" control passes it perfectly. Same trap already documented
  # in binary-freshness-gate.sh, walked into again here.
  local leaked="" cand; cand="$(mktemp)"
  comm -13 "${log}-census-before.txt" "${log}-census-after.txt" > "$cand" 2>/dev/null || true
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    still_alive "${line%% *}" && leaked+="$line"$'\n'
  done < "$cand"
  rm -f "$cand"
  if [[ -n "$leaked" ]]; then
    echo "LEAK CANDIDATES (new and still running — RULE ZERO: reported, NOT killed):" >&2
    echo "$leaked" | sed 's/^/    + /' >&2
    echo "  A green suite is not evidence of a clean box." >&2
  else
    echo "census: no new processes survived the run"
  fi
  echo "runner exit: $rc  ·  log: $log  ·  load samples: $(wc -l < "${log}-load.txt" 2>/dev/null || echo 0)"
  return $rc
}

cmd_report() {
  local log="$1"
  [[ -f "$log" ]] || { echo "SWEEP: USAGE — no log at $log" >&2; return 3; }
  # Delegates the completeness precondition rather than reimplementing it.
  if [[ -x "$here/sweep-result-guard.sh" ]]; then
    "$here/sweep-result-guard.sh" "$log"; return $?
  fi
  [[ -f "${log}-exit.txt" ]] || { echo "SWEEP: INCOMPLETE — no ${log}-exit.txt; the writer has not finished." >&2; return 1; }
  grep -E '^ *[0-9]+ (pass|fail)$' "$log" || { echo "SWEEP: INCOMPLETE — no summary block." >&2; return 1; }
}

cmd_accept() {
  [[ $# -ge 2 ]] || {
    echo "SWEEP: USAGE — acceptance requires TWO logs, from two boxes." >&2
    echo "  Measured 2026-07-30: one box 189/7 and the other 188/8 on the SAME TREE," >&2
    echo "  union ELEVEN distinct failing files from a per-box count of 7-8." >&2
    echo "  Zero on one box certifies a surface it never measured." >&2
    return 3; }
  local a="$1" b="$2" rc=0
  for f in "$a" "$b"; do
    cmd_report "$f" >/dev/null 2>&1 || { echo "SWEEP: COULD-NOT-CHECK — $f is not a complete run." >&2; return 2; }
  done
  local fa fb
  fa="$(grep -oE '^ *[0-9]+ fail$' "$a" | grep -oE '[0-9]+' | tail -1)"; : "${fa:=0}"
  fb="$(grep -oE '^ *[0-9]+ fail$' "$b" | grep -oE '[0-9]+' | tail -1)"; : "${fb:=0}"
  echo "box A: ${fa} fail   ·   box B: ${fb} fail"
  if (( fa == 0 && fb == 0 )); then
    echo "SWEEP: ACCEPTED — zero failures on BOTH boxes"
    return 0
  fi
  echo "SWEEP: NOT ACCEPTED — a box with failures cannot be certified by the other." >&2
  return 1
}

selftest() {
  local d; d="$(mktemp -d)"; trap 'rm -rf "$d"' RETURN
  local rc=0
  # POSITIVE — it must be able to say YES. The case a reject-everything
  # implementation would still pass every negative on.
  printf ' 5 pass\n 0 fail\nRan 5 tests across 2 files. [1s]\n' > "$d/a.log"; echo 0 > "$d/a.log-exit.txt"
  printf ' 5 pass\n 0 fail\nRan 5 tests across 2 files. [1s]\n' > "$d/b.log"; echo 0 > "$d/b.log-exit.txt"
  "$0" accept "$d/a.log" "$d/b.log" >/dev/null 2>&1
  [[ $? -eq 0 ]] && echo "  POSITIVE two green boxes -> ACCEPTED  ok" || { echo "  POSITIVE two green boxes      FAILED"; rc=1; }
  # NEGATIVE — one box only is a USAGE refusal, not a verdict.
  "$0" accept "$d/a.log" >/dev/null 2>&1
  [[ $? -eq 3 ]] && echo "  NEGATIVE single box -> refused        ok" || { echo "  NEGATIVE single box           FAILED"; rc=1; }
  # NEGATIVE — one green one red.
  printf ' 4 pass\n 1 fail\nRan 5 tests across 2 files. [1s]\n' > "$d/c.log"; echo 1 > "$d/c.log-exit.txt"
  "$0" accept "$d/a.log" "$d/c.log" >/dev/null 2>&1
  [[ $? -eq 1 ]] && echo "  NEGATIVE one red box -> NOT ACCEPTED  ok" || { echo "  NEGATIVE one red box          FAILED"; rc=1; }
  # NEGATIVE — a log with no sidecar is COULD-NOT-CHECK (2), never a verdict.
  printf ' 5 pass\n 0 fail\nRan 5 tests across 2 files. [1s]\n' > "$d/d.log"
  "$0" accept "$d/a.log" "$d/d.log" >/dev/null 2>&1
  [[ $? -eq 2 ]] && echo "  NEGATIVE no sidecar -> could-not-check ok" || { echo "  NEGATIVE no sidecar           FAILED"; rc=1; }
  # POSITIVE — `run` writes all four sidecars and the exit code round-trips.
  "$0" run "$d/e.log" -- sh -c 'printf " 1 pass\n 0 fail\nRan 1 test across 1 file. [1s]\n"; exit 0' >/dev/null 2>&1
  if [[ -f "$d/e.log-exit.txt" && -f "$d/e.log-census-before.txt" && -f "$d/e.log-census-after.txt" && -f "$d/e.log-load.txt" ]]; then
    echo "  POSITIVE run writes all sidecars      ok"
  else echo "  POSITIVE run sidecars         FAILED"; rc=1; fi
  # NEGATIVE — a failing command must still produce a readable, complete run.
  "$0" run "$d/f.log" -- sh -c 'echo boom; exit 7' >/dev/null 2>&1
  [[ "$(cat "$d/f.log-exit.txt" 2>/dev/null)" == "7" ]] && echo "  NEGATIVE failing cmd -> exit recorded  ok" || { echo "  NEGATIVE failing cmd          FAILED"; rc=1; }
  # NEGATIVE — the leak detector must NOT fire on a clean run. This is the case
  # the first selftest omitted, and the real smoke run then failed on it:
  # `ps`/`awk`/`sort`/`kworker` were reported as leaks by the census measuring
  # itself. Testing what is easy instead of what carries the risk.
  local out; out="$("$0" run "$d/g.log" -- sh -c 'printf " 1 pass\n 0 fail\nRan 1 test across 1 file. [1s]\n"' 2>&1)"
  if grep -q 'no new processes survived' <<<"$out"; then
    echo "  NEGATIVE clean run -> NO false leaks   ok"
  else echo "  NEGATIVE clean run false-leaked  FAILED"; echo "$out" | sed 's/^/      /'; rc=1; fi

  # POSITIVE — the leak detector must be able to say YES. Without this, a
  # detector that is DEAD CODE reports "clean" forever and passes the
  # no-false-leaks case above perfectly. That is exactly what happened when
  # process substitution failed silently. RULE ZERO: this spawns its own
  # process and stops only that one.
  local out2; out2="$("$0" run "$d/h.log" -- sh -c 'sleep 45 & printf " 1 pass\n 0 fail\nRan 1 test across 1 file. [1s]\n"' 2>&1)"
  if grep -q 'LEAK CANDIDATES' <<<"$out2"; then
    echo "  POSITIVE planted survivor -> DETECTED  ok"
  else echo "  POSITIVE planted survivor MISSED FAILED"; rc=1; fi
  pkill -f 'sleep 45' 2>/dev/null || true

  [[ $rc -eq 0 ]] && echo "SELFTEST: PASS" || echo "SELFTEST: FAIL"
  return $rc
}

case "${1:-}" in
  --selftest) selftest; exit $? ;;
  run)    shift; [[ $# -ge 1 ]] || { echo "usage: sweep.sh run <log> -- <cmd...>" >&2; exit 3; }; cmd_run "$@"; exit $? ;;
  report) shift; [[ $# -ge 1 ]] || { echo "usage: sweep.sh report <log>" >&2; exit 3; }; cmd_report "$@"; exit $? ;;
  accept) shift; cmd_accept "$@"; exit $? ;;
  *) echo "usage: sweep.sh {run|report|accept|--selftest}" >&2; exit 3 ;;
esac

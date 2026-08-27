#!/usr/bin/env bash
# Test-result parser — the one correct way to read a bun or cargo log.
#
# WHY THIS EXISTS
# ---------------
# Two of this run's four counting errors were parsing, not judgement:
#
#   * A grep over `(fail)` LINES double-counted a real 8 into 16. bun prints a
#     `(fail)` line per failing TEST and the summary counts differently; the two
#     are not interchangeable, and the wrong one is bigger, so the error reads
#     as alarming rather than reassuring.
#   * `cargo test` prints `failures:` TWICE — once heading the per-test stdout
#     dumps, once heading the final indented name list. A range anchored on the
#     first block silently returns a SUBSET, which reads as a regression.
#
# Plus two structural traps that make eyeballing unsafe:
#   * bun writes NO `(pass)` lines to a file. A fully-passing log is ~161 bytes
#     and a mid-run log structurally reports zero passes. NEVER classify on byte
#     size, and never infer "nothing passed".
#   * bun prints "Ran 1 test across 1 file" — SINGULAR — for a single-file run.
#     A plural-only regex rejects every single-file gate log. This script's
#     sibling shipped that bug, and all THREE of its negative controls passed
#     happily: a gate that rejects everything satisfies every negative control
#     ever written. Only a POSITIVE control could see it.
#
# HOUSE RULE ON EXIT CODES: "I could not check" never shares a code with "I
# checked and found nothing."
#
#   0  OK           parsed a complete result; counts echoed
#   1  INCOMPLETE   no summary/result block — the run did not finish
#   2  UNKNOWN      cannot tell which runner produced this log
#   3  USAGE        bad invocation / missing file
#
# USAGE
#   scripts/test-result-parse.sh <log>        # auto-detects bun vs cargo
#   scripts/test-result-parse.sh --selftest

set -uo pipefail

parse_bun() {
  local log="$1"
  local pass fail ran
  pass="$(grep -oE '^ *[0-9]+ pass$' "$log" | grep -oE '[0-9]+' | tail -1)"
  fail="$(grep -oE '^ *[0-9]+ fail$' "$log" | grep -oE '[0-9]+' | tail -1)"
  # singular AND plural — see the header note.
  ran="$(grep -E '^Ran [0-9]+ tests? across [0-9]+ files?' "$log" | tail -1)"
  if [[ -z "$pass" || -z "$ran" ]]; then
    echo "PARSE: INCOMPLETE — bun log has no summary block ($log)." >&2
    echo "  A mid-run bun log is a VALID PREFIX: it looks like a finished one." >&2
    return 1
  fi
  : "${fail:=0}"
  echo "runner   bun"
  echo "pass     $pass"
  echo "fail     $fail"
  echo "$ran" | sed 's/^/scope    /'
  # Demonstrate, not merely warn: show what the wrong instrument would have said.
  local failline; failline="$(grep -cE '^\(fail\)' "$log" || true)"
  echo "note     '(fail)' lines in this log: ${failline:-0} — NOT the failure count; the summary says $fail"
  return 0
}

parse_cargo() {
  local log="$1"
  local result
  result="$(grep -E '^test result:' "$log" | tail -1)"
  if [[ -z "$result" ]]; then
    echo "PARSE: INCOMPLETE — cargo log has no 'test result:' line ($log)." >&2
    return 1
  fi
  echo "runner   cargo"
  echo "result   $result"
  echo "failing identities (from '^test .* FAILED\$', never a 'failures:' range):"
  grep -E '^test .* FAILED$' "$log" | sed -E 's/^test (.*) \.\.\. FAILED$/  - \1/' || true
  local blocks; blocks="$(grep -cE '^failures:$' "$log" || true)"
  echo "note     'failures:' header appears ${blocks:-0}x — a range anchored on the first returns a SUBSET"
  return 0
}

selftest() {
  local d; d="$(mktemp -d)"; trap 'rm -rf "$d"' RETURN
  local rc=0

  # POSITIVE 1 — bun, SINGLE file (the singular trap).
  printf ' 1 pass\n 0 fail\n 30 expect() calls\nRan 1 test across 1 file. [15.42s]\n' > "$d/bun1.log"
  "$0" "$d/bun1.log" >/dev/null 2>&1
  [[ $? -eq 0 ]] && echo "  POSITIVE bun single-file (singular)   ok" || { echo "  POSITIVE bun single-file      FAILED"; rc=1; }

  # POSITIVE 2 — bun, many files, WITH (fail) lines that double-count.
  { printf '(fail) e2e a > one [1ms]\n(fail) e2e a > one [1ms]\n'
    printf ' 189 pass\n 7 fail\nRan 196 tests across 124 files. [1482.80s]\n'; } > "$d/bunN.log"
  local out; out="$("$0" "$d/bunN.log" 2>&1)"
  if [[ $? -eq 0 ]] && grep -q 'fail     7' <<<"$out" && grep -q "'(fail)' lines in this log: 2" <<<"$out"; then
    echo "  POSITIVE bun multi + (fail) decoy    ok"
  else echo "  POSITIVE bun multi            FAILED"; rc=1; fi

  # POSITIVE 3 — cargo with a DOUBLED failures: header and panic noise.
  cat > "$d/cargo.log" <<'EOF'
test alpha ... FAILED
test beta ... ok

failures:

---- alpha stdout ----
  left: Some(true)

failures:
    alpha

test result: FAILED. 357 passed; 3 failed; 0 ignored
EOF
  out="$("$0" "$d/cargo.log" 2>&1)"
  if [[ $? -eq 0 ]] && grep -q -- '- alpha' <<<"$out" && grep -q "appears 2x" <<<"$out"; then
    echo "  POSITIVE cargo + doubled header      ok"
  else echo "  POSITIVE cargo                FAILED"; rc=1; fi

  # NEGATIVE 1 — bun prefix, no summary yet.
  printf 'bun test v1.3.10\n(fail) e2e a > one [1ms]\n' > "$d/partial.log"
  "$0" "$d/partial.log" >/dev/null 2>&1
  [[ $? -eq 1 ]] && echo "  NEGATIVE bun mid-run -> INCOMPLETE   ok" || { echo "  NEGATIVE bun mid-run          FAILED"; rc=1; }

  # NEGATIVE 2 — unidentifiable log.
  echo "hello" > "$d/junk.log"
  "$0" "$d/junk.log" >/dev/null 2>&1
  [[ $? -eq 2 ]] && echo "  NEGATIVE unknown runner -> UNKNOWN   ok" || { echo "  NEGATIVE unknown runner       FAILED"; rc=1; }

  # NEGATIVE 3 — missing file.
  "$0" "$d/nosuch.log" >/dev/null 2>&1
  [[ $? -eq 3 ]] && echo "  NEGATIVE missing file -> USAGE       ok" || { echo "  NEGATIVE missing file         FAILED"; rc=1; }

  [[ $rc -eq 0 ]] && echo "SELFTEST: PASS" || echo "SELFTEST: FAIL"
  return $rc
}

[[ "${1:-}" == "--selftest" ]] && { selftest; exit $?; }
[[ $# -ge 1 ]] || { echo "usage: $0 <log> | --selftest" >&2; exit 3; }
log="$1"
[[ -f "$log" ]] || { echo "PARSE: USAGE — no log at $log" >&2; exit 3; }

if grep -qE '^test result:|^running [0-9]+ tests?' "$log"; then
  parse_cargo "$log"; exit $?
elif grep -qE '^ *[0-9]+ (pass|fail)$|^Ran [0-9]+ tests? across|^bun test ' "$log"; then
  parse_bun "$log"; exit $?
fi

echo "PARSE: UNKNOWN — cannot identify the runner that produced $log." >&2
echo "  Refusing to guess. A number parsed from the wrong format is not a result." >&2
exit 2

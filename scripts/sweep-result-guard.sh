#!/usr/bin/env bash
# Sweep-result guard — refuses to report a count from a run that has not
# finished.
#
# WHY THIS EXISTS
# ---------------
# A partial sweep was compared against a finished one and presented as a forming
# discriminator: one box at 53 files against another's 124. It produced a confident
# "accumulated environment state is a first-order term" theory that survived
# until the first box reached its real total and dissolved it.
#
# The author KNEW that box was mid-run, and had flagged that exact error to someone
# else about the other box's numbers TWO MESSAGES EARLIER. A rule articulated
# ten minutes before did not survive ten minutes. That is the whole argument for
# this file existing instead of a note saying "wait for the run to finish".
#
# A `bun test` log in progress is not malformed and not empty. It is a VALID
# PREFIX -- structurally identical to a finished log, just shorter -- so nothing
# about reading it feels wrong. `bun test` also writes no `(pass)` lines to a
# file, so a mid-run log structurally reports zero passes and a fully-passing
# log is ~161 bytes. NEVER classify on byte count.
#
# EXIT CODES ARE DISTINCT so a control can tell "refused correctly" from "died":
#
#   0  COMPLETE    exit file present; summary parsed and echoed
#   1  INCOMPLETE  no exit file, or no summary line -- the writer has not finished
#   3  USAGE       bad invocation / log missing entirely
#
# USAGE
#   scripts/sweep-result-guard.sh <log-path>
#
# CONVENTION: the runner writes "<log>-exit.txt" containing the exit status,
# AFTER the test process returns:
#   TMPDIR=/root/tmp-e2e nice -n 19 bun test --max-concurrency=1 tests/e2e > "$LOG" 2>&1
#   echo $? > "$LOG-exit.txt"

set -uo pipefail

[[ $# -ge 1 ]] || { echo "usage: $0 <log-path>" >&2; exit 3; }
log="$1"

[[ -f "$log" ]] || { echo "SWEEP-GUARD: USAGE — no log at $log" >&2; exit 3; }

exit_file="${log}-exit.txt"
if [[ ! -f "$exit_file" ]]; then
  echo "SWEEP-GUARD: INCOMPLETE — refusing to report a count." >&2
  echo "  $exit_file is absent, so the writer has not finished." >&2
  echo "  A mid-run log is a VALID PREFIX, not a malformed file: it looks exactly" >&2
  echo "  like a finished one and will give you a smaller, confident, wrong number." >&2
  exit 1
fi

# The summary block is the only trustworthy source. Never a `(fail)` line count:
# a grep over those double-counted a real 8 into 16 tonight.
# NOTE the singular/plural: bun prints "Ran 1 test across 1 file." for a single
# test and "Ran 196 tests across 124 files." otherwise. A plural-only regex
# rejects every single-file gate log — caught by this script's own POSITIVE
# control, which is the case people skip because they "know" it passes.
summary="$(grep -E '^ *[0-9]+ (pass|fail)$' "$log" || true)"
ran_line="$(grep -E '^Ran [0-9]+ tests? across [0-9]+ files?' "$log" || true)"

if [[ -z "$summary" || -z "$ran_line" ]]; then
  echo "SWEEP-GUARD: INCOMPLETE — exit file present but no summary block in $log." >&2
  echo "  The run was killed or the log truncated. An aborted sweep is not a result." >&2
  exit 1
fi

echo "SWEEP-GUARD: COMPLETE — $log"
echo "  runner exit status: $(tr -d '[:space:]' < "$exit_file")"
echo "$summary" | sed 's/^/  /'
echo "  $ran_line"
echo "  (counts read from the SUMMARY BLOCK, never from (fail) lines, never from byte size)"
exit 0

#!/usr/bin/env bash

# Count only the requested company's live ChiefD argv. Callers provide the
# installed binary path and the company DIRECTORY — the one thing `chiefd run`
# is told, and the company's identity; unrelated companies on adjacent
# directories must never make a single-company deploy look duplicated.
#
# `([[:space:]]|$)` is load-bearing, not tidiness. `pgrep -f` matches an ERE
# against a whole argv, and `--dir …/companies/acme` is a PREFIX of
# `--dir …/companies/acme-corp` — the same collision the tmux session
# terminator exists to make impossible, and under one companies root those two
# directories are the ordinary case rather than a contrived one. Without the
# anchor a single-company deploy would count a SIBLING company's daemon and
# refuse itself.
company_chiefd_instance_count() {
  local binary_name company_dir pattern
  binary_name="$(basename "$1")"
  company_dir="$2"
  pattern="${binary_name} run --dir ${company_dir}([[:space:]]|$)"
  pgrep -af -- "$pattern" 2>/dev/null | wc -l | tr -d ' '
}

#!/usr/bin/env bash
# chiefd-backup.sh — snapshot beacond's registry and every per-company database.
#
# #751/G13 corrected this header. It said chiefd's durable state was "a global
# `registry.db`" — a file D21 says cannot exist and this script has never
# touched: what it actually snapshots, and always did, is beacond's own store
# ($BEACOND_REGISTRY, ~/.chief/beacond.sqlite, the Mandate-5 exception
# recorded at DECISIONS.md:1236). The mechanism below was right; only the
# sentence explaining it named a retired thing, which is worse than saying
# nothing at all — an operator reading this at 3am would go looking for a file
# that is not there.
#
# The real shape: discovery lives in beacond's single-table store, and company
# state lives in ONE DATABASE PER COMPANY, inside the company's own directory
# at `<dir>/.chief/db/chief.db`. A backup that snapshotted only one file would
# silently miss every company DB and still exit 0. That is the specific failure
# this script exists to prevent: a backup that looks like it worked and
# restores to nothing. The registry and each company database are snapshotted
# independently.
#
# A SNAPSHOT IS NAMED AFTER ITS COMPANY'S DIRECTORY, AND THAT FIXES A COLLISION
#
# This used to write `companies/<slug>.db`, and a slug names no company: two
# companies under different orgs roots could share one, so the second snapshot
# silently OVERWROTE the first and the backup restored one company twice. A
# company is a directory now, and on a deployed box every company is a sibling
# under one companies root — so directory names are unique by construction and
# the basename cannot collide. The name a backup file carries is therefore the
# directory's, taken from the registry row, never the display slug beside it.
#
# THE REGISTRY IS SNAPSHOTTED FIRST, AND THAT ORDER IS LOAD-BEARING
#
# The registry is the sole authority for which companies exist and where they
# live (plan §5.7). A company database recovered WITHOUT its registry row is
# unattributable: you have a file full of assignments and goals and no record
# of which directory it belongs to. Backing the registry up first means every
# later snapshot in the run has something to be interpreted by, even if the run
# is interrupted halfway.
#
# The reverse is survivable: a registry row naming a company whose snapshot did
# not complete tells you exactly what is missing, by name. Knowing what you lost
# is worth a great deal more than it sounds like at 3am.
#
#   scripts/chiefd-backup.sh backup [dir]    registry + every company DB
#   scripts/chiefd-backup.sh verify <dir>    integrity-check a backup directory
#
# Snapshots use SQLite's `VACUUM INTO`, which takes a transactionally
# consistent copy of a LIVE database. `cp` on a database with WAL writes in
# flight is not safe and produces a file that usually restores and sometimes
# does not, which is the worst of both.

set -euo pipefail

# The box's own chief directory, `~/.chief` — where beacond's registry lives
# (`beacond::config`'s documented default) alongside the binaries and the
# `launcher-root` pointer. `~/.chiefd` was the retired global company tree.
INSTALL_HOME="${INSTALL_HOME:-$HOME/.chief}"
BEACOND_REGISTRY="${BEACOND_DB_PATH:-$INSTALL_HOME/beacond.sqlite}"
BEACOND_URL="${BEACOND_URL:-http://127.0.0.1:8790}"
BACKUP_ROOT="${CHIEFD_BACKUP_DIR:-$INSTALL_HOME/backups}"

die() { echo "chiefd-backup: $*" >&2; exit 1; }

need_sqlite() {
  command -v sqlite3 >/dev/null 2>&1 || die "sqlite3 is required and not on PATH"
}

# Every company's DIRECTORY, one per line. The directory is the identity
# beacond keys its rows by; the `slug` beside it is a display word two rows may
# share, and this script's output file names must not.
companies() {
  curl --fail --silent --show-error --max-time 2 "$BEACOND_URL/v1/list" \
    | python3 -c 'import json,sys; [print(c["dir"]) for c in json.load(sys.stdin)["companies"]]'
}

snapshot() {
  local src="$1" dest="$2"
  # Two distinct failure shapes, distinguished by exit status: 2 means "no
  # database file at all" (see the #768 note at the missing-company site
  # below — this is the ordinary residue of an interrupted create, not a
  # backup failure); 1 means the database exists but VACUUM INTO failed
  # against it (a real, page-a-human failure).
  [ -f "$src" ] || return 2
  sqlite3 "$src" "VACUUM INTO '$dest'" || return 1
}

case "${1:-}" in
  backup)
    need_sqlite
    dir="${2:-$BACKUP_ROOT/$(date -u +%Y%m%dT%H%M%SZ)}"
    mkdir -p "$dir/companies"
    [ -f "$BEACOND_REGISTRY" ] || die "no beacond registry at $BEACOND_REGISTRY — nothing to back up"
    company_dirs="$(companies)" || die "beacond at $BEACOND_URL is unreachable — refusing an incomplete backup"

    # 1. The registry FIRST. See the header.
    snapshot "$BEACOND_REGISTRY" "$dir/beacond.sqlite" || die "beacond registry snapshot failed"

    # 2. Every company named by the registry we just captured, so the backup
    #    is internally consistent rather than a mix of two moments.
    missing=0
    failed=0
    total=0
    # Read into a variable rather than `< <(…)`: process substitution needs
    # /dev/fd, which this deploy host does not provide, and a backup script
    # that only runs on some hosts is not a backup script.
    while IFS= read -r company_dir; do
      [ -n "$company_dir" ] || continue
      total=$((total + 1))
      db="$company_dir/.chief/db/chief.db"
      name="$(basename "$company_dir")"
      # `&&` rather than `if …; then …; fi`: with no `else`, a failed `if`
      # condition still leaves the `if` STATEMENT's own exit status at 0 (no
      # branch ran), so `$?` right after `fi` silently reads 0 regardless of
      # which way snapshot failed. `snapshot … && continue` short-circuits on
      # failure, leaving `$?` as snapshot's real exit status below.
      snapshot "$db" "$dir/companies/$name.db" && continue
      status=$?
      if [ "$status" -eq 2 ]; then
        # Creation order is createCompany FIRST, then genesis, then the Pi
        # home (E7-S4, ruling D24/F27): a registered company with no database
        # file yet is the ordinary residue of an interrupted create, not data
        # loss. It is fixed by re-creating the company in that directory, and
        # no code anywhere may hunt for it or repair it (ruling D19/D21).
        # Report it by name so a human can decide, but do not fail the backup
        # over it — a company that legitimately has nothing to back up yet is
        # not the "looks like it worked and restores to nothing" failure this
        # script exists to catch.
        echo "chiefd-backup: SKIP no database yet for '$company_dir' at $db (uncreated company — not data loss; re-create the company in that directory if this is unexpected)" >&2
        missing=$((missing + 1))
        continue
      fi
      # The database DOES exist but VACUUM INTO failed against it — this is
      # the real, page-a-human failure the exit code below is reserved for.
      echo "chiefd-backup: FAILED snapshot for '$company_dir' at $db" >&2
      failed=$((failed + 1))
    done <<EOF
$company_dirs
EOF

    printf '{"backup":"%s","registry":"%s","companies":%d,"missing":%d,"failed":%d}\n' \
      "$dir" "$dir/beacond.sqlite" "$total" "$missing" "$failed"
    # Non-zero is reserved for the two real failures: beacond unreachable
    # (already `die`d above) and a VACUUM INTO that fails on a database that
    # DOES exist. A registered-but-not-yet-created company is not a backup
    # failure — see the SKIP branch above.
    [ "$failed" -eq 0 ] || exit 1
    ;;

  verify)
    need_sqlite
    dir="${2:?usage: chiefd-backup.sh verify <backup-dir>}"
    [ -f "$dir/beacond.sqlite" ] || die "$dir has no beacond.sqlite"
    bad=0
    for db in "$dir/beacond.sqlite" "$dir"/companies/*.db; do
      [ -f "$db" ] || continue
      result="$(sqlite3 "$db" 'PRAGMA integrity_check' || echo "unreadable")"
      if [ "$result" != "ok" ]; then
        echo "chiefd-backup: FAILED $db: $result" >&2
        bad=$((bad + 1))
      fi
    done
    # Cross-check: every company the backed-up registry names must have a
    # snapshot. A backup that verifies file-by-file while missing a company
    # entirely would pass a per-file check and restore to a smaller fleet.
    # Keyed on `dir`, the registry's own identity column, so this asks the same
    # question the backup answered rather than a display name's version of it.
    named="$(sqlite3 "$dir/beacond.sqlite" "SELECT dir FROM companies")"
    while IFS= read -r company_dir; do
      [ -n "$company_dir" ] || continue
      [ -f "$dir/companies/$(basename "$company_dir").db" ] || {
        echo "chiefd-backup: FAILED registry names '$company_dir' but no snapshot exists" >&2
        bad=$((bad + 1))
      }
    done <<EOF
$named
EOF
    [ "$bad" -eq 0 ] || exit 1
    echo "{\"verified\":\"$dir\"}"
    ;;

  *)
    sed -n '1,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac

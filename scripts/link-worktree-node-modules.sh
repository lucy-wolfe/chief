#!/usr/bin/env bash
#
# Make a worktree's `node_modules` describe THIS checkout.
#
# # The trap this removes
#
# A worktree normally borrows `node_modules` from the tree it was made from,
# usually by symlinking the whole directory. That works for third-party
# packages and is wrong for this repo's own: `node_modules/@chief/piing` is a
# RELATIVE link (`../../packages/piing`), so through a borrowed `node_modules`
# it resolves into the SHARED tree's `packages/` — another agent's working
# copy, at another commit.
#
# Anything that reads one half of the repo by relative path and the other by
# package name is then comparing two revisions of the repo. Measured:
# `tool-surface-artifact.test.mjs` reported `missing: ['org_resume',
# 'org_stand_down']` — "a hosted CEO is granted tools the host cannot build" —
# against code that was shipped, correct, and green in CI. Three agents were
# sent after that phantom before anybody looked at the link.
#
# # What it does
#
# For every directory that needs one, replaces the borrowed `node_modules` with
# a REAL directory: every entry linked straight through to the shared tree's
# copy, EXCEPT `@chief`, whose members point at this worktree's own
# `packages/*`. Third-party packages stay shared — they are large, identical,
# and not the subject — while every `@chief/*` import resolves to the code you
# are actually testing.
#
# Idempotent: safe to run in a worktree that is already correct, and safe to
# run again after `git checkout` of another branch. Run it reflexively.
#
#   scripts/link-worktree-node-modules.sh [source-checkout]
#
# `source-checkout` is the tree to borrow third-party packages from. It
# defaults to the worktree's main checkout, which `git worktree list` names
# first.
set -euo pipefail

worktree="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

if [[ $# -ge 1 ]]; then
  source_root="$(cd "$1" && pwd -P)"
else
  # The main checkout is the first line of `git worktree list`, which is where
  # a `bun install` has actually been run.
  source_root="$(git -C "$worktree" worktree list --porcelain | awk '/^worktree /{print $2; exit}')"
fi

if [[ "$source_root" == "$worktree" ]]; then
  echo "REFUSED: $worktree is the main checkout, not a worktree." >&2
  echo "  Its node_modules is the real one — run bun install there instead." >&2
  exit 1
fi

if [[ ! -d "$source_root/node_modules" ]]; then
  echo "REFUSED: $source_root has no node_modules to borrow from." >&2
  echo "  Run bun install there first." >&2
  exit 1
fi

# Every directory the source checkout has a node_modules for. Derived, never
# hand-listed: a package that gains one must not need this script edited, and a
# hand-listed set is exactly how four of six got mirrored and two did not —
# which left `bash scripts/typecheck.sh` failing with ~20 TS2307 errors that
# read like missing types and were a missing directory.
# A here-string rather than process substitution: `< <(...)` needs /dev/fd,
# which some containers do not mount, and this script has to run wherever a
# worktree does.
packages="$(
  cd "$source_root" &&
    find . -maxdepth 3 -name node_modules -not -path '*/node_modules/*' -print |
    sed 's|^\./||; s|node_modules$||; s|/$||' | sort
)"

if [[ -z "$packages" ]]; then
  echo "REFUSED: found no node_modules under $source_root." >&2
  exit 1
fi

link_one() {
  local relative="$1"
  local source="$source_root/${relative:+$relative/}node_modules"
  local target="$worktree/${relative:+$relative/}node_modules"

  if [[ ! -d "$worktree/${relative:-.}" ]]; then
    echo "REFUSED: $source_root has $relative/node_modules but this worktree has no $relative." >&2
    echo "  The two checkouts disagree about which packages exist; rebase before linking." >&2
    return 1
  fi

  # Idempotent by REBUILD rather than by patching in place: a stale entry from
  # a previous branch is invisible to a patch and would keep resolving.
  rm -rf "$target"
  mkdir -p "$target"

  local entry name
  shopt -s nullglob dotglob
  for entry in "$source"/*; do
    name="$(basename "$entry")"
    [[ "$name" == "@chief" ]] && continue
    ln -s "$entry" "$target/$name"
  done
  shopt -u nullglob dotglob

  # `@chief/*` at THIS worktree's own packages. Named from the source tree's
  # own set so a package added to the workspace appears here by itself.
  if [[ -d "$source/@chief" ]]; then
    mkdir -p "$target/@chief"
    for entry in "$source"/@chief/*; do
      name="$(basename "$entry")"
      if [[ ! -d "$worktree/packages/$name" ]]; then
        echo "REFUSED: @chief/$name is a workspace package and this worktree has no packages/$name." >&2
        return 1
      fi
      ln -s "$worktree/packages/$name" "$target/@chief/$name"
    done
  fi
  echo "  ${relative:-.}/node_modules"
}

echo "Borrowing third-party packages from $source_root"
echo "Pointing @chief/* at $worktree/packages"
echo "Mirrored:"
while IFS= read -r relative; do
  link_one "$relative"
done <<< "$packages"

# VERIFY, because a setup script that silently half-works is the failure it
# exists to prevent. Every `@chief` link in the worktree must now resolve
# inside the worktree — one foreign link is enough to make a guard compare two
# checkouts, which is the whole point of the exercise.
foreign=0
chief_links="$(find "$worktree" -path '*/node_modules/@chief/*' -maxdepth 5 -mindepth 1 -type l)"
if [[ -z "$chief_links" ]]; then
  echo "FAILED: no @chief/* links were created at all." >&2
  exit 1
fi
while IFS= read -r link; do
  resolved="$(readlink -f "$link")"
  if [[ "$resolved" != "$worktree"/* ]]; then
    echo "FAILED: $link resolves to $resolved, outside this worktree." >&2
    foreign=1
  fi
done <<< "$chief_links"

if [[ $foreign -ne 0 ]]; then
  echo "FAILED: some @chief packages still resolve to another checkout." >&2
  exit 1
fi

echo "Verified: every @chief/* link resolves inside this worktree."

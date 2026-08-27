---
name: software-development-process
description: 'The standing process for any significant change in this repo: branch off main into an isolated worktree, write the plan first as a local working document (git-ignored, never committed), implement on that branch, run the full standing check list, then push and hand the PR back for the human to merge. Invoke when the user asks for a feature, fix, or refactor to be taken from agreement to a reviewable PR — especially when they say "do the work in a worktree", "create a plan", "push to PR", or name this process.'
---

# software-development-process

One piece of work, one worktree, one branch, one PR. The human merges. You never
do.

## Phase 1 — Worktree off main

Create the worktree from **main**, never from the current branch, and never in
the primary checkout:

```
git -C <repo> fetch origin main
git -C <repo> worktree add -b <slug> ~/worktrees/<slug> origin/main
```

`~/worktrees/<slug>/` is the mandated location for every worktree and build in
this repo. `<slug>` is kebab-case and names the change, not the defect.

Every command from here runs inside `~/worktrees/<slug>/`.

## Phase 2 — Plan first (a local working document)

Write `plans/<slug>.md` containing, in this order:

1. A **four-to-five-sentence TL;DR**. What is broken, what changes, why this
   shape and not the obvious alternative.
2. **Scope** — and an explicit non-scope list. What this change deliberately
   does not touch.
3. **Acceptance criteria** — each one checkable by a person reading the diff.
   Ordering constraints belong here, stated as criteria, not as prose buried in
   the implementation notes.
4. **Implementation checklist** — the ordered steps.
5. **Risks** — every one you already know about. A risk discovered during
   investigation and left out of the plan is a defect in the plan.

Write this plan **before any implementation code**. `plans/` is git-ignored
and **never committed** — the plan is a LOCAL working document, not a PR. It is
the contract you hold yourself to and the record of why the code is the way it
is; keep it beside you as you implement.

## Phase 3 — Implement on that branch

Keep the plan current. When a verified fact contradicts it, edit the plan as
you learn — a plan that disagrees with the code at the end of the run is a
failed run.

Standing rules that apply to every commit here:

- **Every code change carries tests, and unit tests that lock in business
  logic are non-negotiable.** New behavior gets unit tests that pin the RULE it
  implements, not merely that the code runs; a bug fix gets a regression test
  that fails before the fix. A change whose behavior no test would catch
  regressing is not finished.
- **Existing tests are a contract.** Never weaken, skip, or delete a test to
  make a change pass. A refactor that preserves behavior keeps its existing
  assertions green **unchanged** — that is the proof it was a refactor.
- **No backward-compatibility layers.** Remove the obsolete path; do not add a
  fallback or a migration beside it.
- **Cross-platform.** Type-check macOS *and* Linux before handoff. Never write
  code that only compiles on the author's machine.
- Commit in intentional steps with real messages. Not one commit at the end.

## Phase 4 — The standing checks, by name

Run all six from the worktree root. Do not substitute a subset:

```
bun run typecheck
bun run test
bun run lint
bun run lint:reactive
bun run knip
bun run test:pre-push-guards
```

The last one covers none of what the first five cover, which is exactly why it
is forgotten. Two of its subtests fail for reasons about the machine rather than
the change — a `sql-only-state.test.mjs` subtest that assumes no host-level git
identity is configured, and `gate-matrix-sequence.test.mjs`, which only holds
under `CI=1`. Check a failing subtest's own name against those two before
calling it a regression. Anything else red is red.

Also update, in the same branch:

- `CHANGELOG.md` — the delivered behavior.
- `DECISIONS.md` — one concise dated line per product, UX, security, workflow,
  or architecture choice made during the work.

## Phase 5 — Hand back, do not merge

Push the branch. Report to the human with:

- the PR URL;
- each acceptance criterion and whether it is met;
- the standing checks and their real results — if something is red, say so and
  paste the output;
- anything left undone, named explicitly, with the reason.

**Then stop.** Do not merge, do not squash, do not push to main. The human
merges. Reporting a check as passing when it was not run is the one
unrecoverable failure of this process.

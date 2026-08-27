# Tracking board — pluggable

Pick the board mode during the question gate (it is one of the standard
questions: board and label conventions, and whether auth scopes exist). The
mode never blocks the run — degrade rather than stall.

## Mode A (default): GitHub Projects

Requires a token with the project scope. Verify BEFORE the question gate closes:

```
gh auth status
gh project list --owner <OWNER>        # fails cleanly if scope is missing
```

If the scope is missing, that is a question-gate question (ask the user to run
`gh auth refresh -s project`, or fall back to Mode B).

Command shapes (all values are per-run parameters, discovered at run start —
never reuse IDs from a previous run):

```
# Discover the project and its Status field/option ids
gh project list --owner <OWNER>
gh project field-list <PROJECT_NUMBER> --owner <OWNER> --format json

# Create a SMALL ticket and add it to the board
gh issue create --title "<title>" --label <PLAN_LABEL> --body "<complete instructions>"
gh project item-add <PROJECT_NUMBER> --owner <OWNER> --url <issue-url>

# BIG ticket: the design-doc PR is the board item
gh pr create --draft --title "<title>: design" --label <PLAN_LABEL> --body "Design doc only. Never merge."
gh project item-add <PROJECT_NUMBER> --owner <OWNER> --url <pr-url>

# Move an item's status (Todo / In Progress / Done)
gh project item-edit --id <ITEM_ID> --project-id <PROJECT_ID> \
  --field-id <STATUS_FIELD_ID> --single-select-option-id <OPTION_ID>

# Reconcile: list everything with current status
gh project item-list <PROJECT_NUMBER> --owner <OWNER> --format json
```

Example only (do not reuse): a run might resolve to
`gh project item-list 7 --owner example-org --format json`.

## Mode B (degraded): labeled issues only

Use when Projects is unavailable or unauthorized. Same tickets, no project
board; status is carried by labels:

```
gh label create <PLAN_LABEL> 2>/dev/null || true
gh label create status:in-progress 2>/dev/null || true
gh issue create --title "<title>" --label <PLAN_LABEL> --body "<instructions>"
gh issue edit <N> --add-label status:in-progress
gh issue close <N> --comment "Done: <what landed, merge SHA>"   # closed == Done
gh issue list --label <PLAN_LABEL> --state all                  # reconcile
```

## Mode C (last resort): local checklist file

Use when there is no usable issue tracker at all (no auth, no remote). Keep a
committed checklist per plan at `plans/<plan-slug>-board.md`:

```
# Board: <plan-slug>
- [ ] T1 — <title> — Blocked by: nothing — status: todo
- [ ] T2 — <title> — Blocked by: T1 — status: todo
```

Rules: one line per ticket; ticket bodies (the complete instructions) live in
`plans/<plan-slug>-tickets/T<N>.md`; the merger updates status and checks the
box as part of each landing commit; "all boxes checked" is the release
condition. BIG tickets keep their design doc at `plans/<slug>.md` even in this
mode (the PR requirement drops away only if there is no remote to push to).

## Invariants in every mode

- Every item carries the per-plan label (or lives in the per-plan file) so
  plans stay distinguishable.
- Every item has an explicit "Blocked by:" line.
- Every item ends in Done (closed / checked). Done means done.
- The operator reconciles the board every 15-minute beat.

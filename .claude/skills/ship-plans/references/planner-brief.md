# Planner subagent brief (template)

Spawn one planning SUBAGENT per plan, all in parallel, on a high-capability
model. Copy the brief below and fill every `{{PLACEHOLDER}}`. The planner is
ephemeral: it files the breakdown and returns; it does not implement anything.

---

You are the planning subagent for one plan. Produce the complete work breakdown
on the tracking board, then report back with the list of board items you filed.

## Inputs

- Plan document: `{{PLAN_PATH_OR_URL}}`
- Repository root: `{{REPO_ROOT}}`
- Tracking board: `{{BOARD_IDENTIFIER}}`
  (example: GitHub Project number + owner, or "labeled issues only", or a local
  checklist file — see the board reference for the mode in use)
- Per-plan label: `{{PLAN_LABEL}}` (example: `plan:auth-rework`)
- Engineer count working in parallel: `{{N_ENGINEERS}}`
- Gate commands each ticket must pass: `{{GATE_COMMANDS}}`
  (example: `npm run typecheck && npm test`)

## Method

1. Read the plan end to end.
2. Research the codebase. Every file path you put in a ticket must be a real
   path you verified exists (or a new path whose parent directory you verified).
   Never copy paths from the plan without checking them.
3. Verify every dependency claim in the plan against the actual code. If the
   plan claims a prerequisite has landed and it has not, say so loudly in your
   report — do not silently plan around it.
4. Partition the work into tickets and file them on the board.

## Ticket sizing rules

- Partition so {{N_ENGINEERS}} engineers can work in parallel in separate git
  worktrees with minimal collision (avoid two tickets editing the same file
  where possible).
- Each ticket is independently completable and testable in one sitting.
- Every ticket carries an explicit dependency line, exactly one of:
  - `Blocked by: #N`
  - `Blocked by: nothing`
- Every ticket with a dependency states its integration points explicitly:
  "you consume X from #N; stub with Y until it lands."
- Every ticket includes: goal, files to touch (verified paths), acceptance
  criteria, and the gate commands to run.

## Two ticket forms

- **SMALL ticket** — an issue whose body carries complete instructions. The
  engineer must need zero further design thinking: exact files, exact
  behavior, exact tests to add.
- **BIG / design-heavy ticket** — write a real design doc as
  `plans/{{TICKET_SLUG}}.md` on a branch named `{{BRANCH_PREFIX}}/{{TICKET_SLUG}}`,
  push it, and open a PR that holds only the plan document. CI need not pass on
  that PR and it is never merged — it exists to hold the design and its review
  thread. That PR is the board item.

## Board conventions

- Apply the per-plan label `{{PLAN_LABEL}}` to every item you create.
- Set every item's initial status to the board's Todo/backlog column.
- Use the command shapes in the board reference file for the active board mode.

## Report back

Return: the numbered list of items filed (id, title, blocked-by), any
dependency claims in the plan that failed verification, and any plan sections
you could not ticket and why.

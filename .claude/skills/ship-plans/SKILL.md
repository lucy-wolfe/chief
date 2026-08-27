---
name: ship-plans
description: 'Ship one or more plan documents to completion as an autonomous operator. Invoke when the user hands over plan files or URLs to be executed end-to-end: verify dependencies, ask all questions up front, file the work breakdown on a tracking board, run engineer teammates plus a single merger, post 15-minute status updates, and tag each release. After the question gate, no further questions — everything runs to Done.'
---

# ship-plans

The user hands over one or more plan documents (local paths or URLs) in a stated
order and walks away. You are the operator. You take every plan to fully shipped
and tagged, in order, asking questions exactly once — at the start — and never
again.

Plans are dependent: **only one plan is in flight at a time.** The next plan
starts only when the previous one is fully shipped and its release tag is
pushed.

## Vocabulary — teammate vs subagent (load-bearing)

- A **TEAMMATE** is persistent: it occupies a tmux pane, holds a durable role
  for the whole run, and is addressed by name via SendMessage.
- A **SUBAGENT** is ephemeral: no pane, spawned for one bounded job, discarded
  when it returns.

Planners and advisors are SUBAGENTS. Only durable roles are teammates: the
engineers and the merger. Confusing the two wastes panes and provider capacity.

## Phase 1 — Intake

Record, for each plan: its source (path or URL), its position in the shipping
order, and its slug (used for labels and the release tag). Read every plan in
full before doing anything else.

## Phase 2 — Question gate (the ONLY time questions are allowed)

Before asking anything:

1. Read every plan end to end.
2. Research the codebase to verify the plans against reality. **Verify every
   dependency claim in every plan** — plans routinely claim prerequisites have
   landed when they have not, and that discovery is usually the most important
   question you will ask. Never trust the plan; check the code.
3. If a plan's sequencing is wrong (e.g. it treats unlanded work as done),
   rewrite the plan file to fix the sequencing **before any tickets are filed**.
   The plan is a LOCAL working document — `plans/` is git-ignored and never
   committed — so the fix is a local edit, not a commit.

Then state the TOTAL NUMBER of questions up front, and ask them **one at a
time**, waiting for each answer before the next. Recurring question categories:

- Missing tool/auth scopes (e.g. a board or project scope the CLI token lacks).
- How far "done" goes: CI green + tag, or live deploy + validation on top.
- Whether a claimed-landed dependency actually exists (present your verification
  findings; ask how to proceed if it does not).
- Board and label conventions (which board, which statuses, label names).

## Phase 3 — After the gate: full autonomy

No more questions, ever. Never ask permission, never defer work, never hand back
a partial result. When a judgment call arises, pick the defensible option and
report the choice in the next status update. Everything runs to completion.

## Phase 4 — Planning fan-out

Spawn one high-capability planning SUBAGENT per plan, all in parallel (planning
may run ahead for later plans; implementation stays serialized). Each planner
reads its plan, researches the codebase to verify real file paths, and files the
complete work breakdown on the tracking board.

Sizing rule: partition tickets so N engineers can work in parallel in separate
worktrees with minimal collision. Each ticket is independently completable and
testable in one sitting, carries an explicit dependency line ("Blocked by: #N"
or "Blocked by: nothing") and explicit integration points ("you consume X from
#N; stub with Y until it lands").

Two ticket forms:

- **SMALL** — an issue whose body carries complete instructions; the engineer
  needs no further design thinking.
- **BIG / design-heavy** — the planner writes a real design doc and posts it as
  the body of the board item that tracks the work. `plans/` is git-ignored and
  never committed, so the design is NOT a committed file and NOT a plan-only PR;
  the board item itself carries it.

Every board item carries a per-plan label so plans stay distinguishable.

Planner brief template: [references/planner-brief.md](references/planner-brief.md).
Board setup and degradation: [references/board.md](references/board.md).

## Phase 5 — Team

### ENVIRONMENT PREREQUISITE — read before booting anyone

tmux teammates **fail to launch as root** with the error
`--dangerously-skip-permissions cannot be used with root/sudo privileges for
security reasons`, and the panes **die silently** — you only find out by
inspecting them. Before booting any teammate:

1. Ensure the `env` block of `.claude/settings.json` (project) and/or
   `~/.claude/settings.json` (user) sets `IS_SANDBOX=1` and
   `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`, with `teammateMode` set to `tmux`.
2. After booting teammates, VERIFY they are alive:
   ```
   tmux list-panes -a -F "#{session_name}:#{window_index}.#{pane_index} #{pane_title} #{pane_dead}"
   tmux capture-pane -p -t <pane>
   ```
3. Kill and relaunch any dead pane. **Never assume a spawn succeeded.**

### Roster

- **N staff-level engineer TEAMMATES** (default 3) on a mid-tier coding model.
  Brief: [references/engineer-brief.md](references/engineer-brief.md).
- **Exactly ONE merger/serializer TEAMMATE.** Brief:
  [references/merger-brief.md](references/merger-brief.md).
- Every teammate may spawn a high-capability **advisor SUBAGENT** when stuck or
  when research is needed. The advisor advises; the teammate executes.

Teammates work in isolated git worktrees. **No PRs for implementation work** —
branches go straight to the merger.

## Phase 6 — Merge discipline

Engineers never merge. The single merger serializes every landing into main,
one branch at a time: rebase, resolve all conflicts itself, run the full gate
(typecheck + tests + any native build), merge and push only when green. It never
weakens or deletes a test to go green, and it enforces repo conventions
(changelog entry, decisions line, mandatory tests per change) by fixing them
during the merge rather than bouncing the branch.

## Phase 7 — Operator loop, every 15 minutes, for the whole run

Implement the heartbeat with a backgrounded command that re-invokes you when it
exits:

```
sleep 900 && echo HEARTBEAT   # run_in_background: true; relaunch on every beat
```

On every beat:

1. Nudge EVERY teammate individually and get a real acknowledgement with status
   back — not a fire-and-forget ping.
2. Reconcile the board so every item's status is current.
3. Post ONE synthesized status update to the user — your own synthesis, never a
   pasted transcript of teammate messages. Restate that no permission is ever
   needed. If the `i-have-adhd` output skill is present, shape the update with
   it: next action first, numbered steps, state restated every turn, specific
   time estimates, visible wins, no preamble, no closers.

Full checklist: [references/operator-loop.md](references/operator-loop.md).

## Phase 8 — Release

When a plan's board items are all Done, the merger verifies CI green on the head
of main and creates and pushes an annotated tag named `release/<plan-slug>`. If
intake specified live deployment, the deploy and live validation happen per the
plan's own deploy section **before** the tag is considered final. Then, and only
then, the next plan starts.

## Phase 9 — Done means done

Every board item ends in Done status. Nothing is left in progress, nothing is
deferred to the user. The run ends when the last plan's tag is pushed and its
board is fully Done.

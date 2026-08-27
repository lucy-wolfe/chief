# Verification rules (#497, #498)

Two systemic failure modes, each responsible for multiple false readings in a
single day (2026-07-24). Both are the same family as CLAUDE.md's "Verify the
artifact, not the intent" — this file states them operationally and links the
places they have been mechanized so the next person reaches for the guard, not
the aspiration.

## Rule 1 — `tested` and `reached` are independent (#497)

A green test proves the code **works when called**. It says nothing about whether
anything **calls it**. Reading "tests pass" as "the feature is live" conflates two
different claims. Unreached code emits **no signal at all** — `grep` finds callers
and stops the investigation early, so partial-reach is harder to see than a wholly
dead module.

**Operationally, before claiming a change is live:**
1. Name the caller path that reaches the code you changed. "There is a test" is
   not "there is a caller."
2. Reach can be a **per-call-path** property within one file, not just per-file.
   The same function can be inert on one path and live on another (see #500:
   `org-supervision.ts` is chiefd's on the daemon path, live TS on the
   assignment-command path).
3. An absence is only evidence if you can say WHERE you looked and prove the
   search could have returned a positive — run the positive control (a naive
   `grep -rl` that hits `.claude/worktrees/*` copies is a positive-control
   failure).

**Mechanized instances (guards that fail if reach silently changes):**
- #506 — deploy-verify NAMES a running pane that cannot have loaded its current
  extensions (reached-but-stale is surfaced, not assumed live).

**Where a general "dead export" detector would go:** a lint listing `src/` exports
with no non-test, non-comment importer in the main tree would mechanize the
whole-module case (the issue names `org-memory-review-store.ts` as genuinely
unreached). Not built yet — it is noise-prone (entry points look dead) and needs a
curated allowlist; tracked here so it is a decision, not an omission.

## Rule 2 — a message states what the check ACTUALLY verifies (#498)

A refusal/success/status message must state what the code **actually checks**,
never what it **aspires to**. The message is the only interface most people have
to the check, so a message that overstates is functionally a check that does not
exist — and worse, it discourages the verification it pre-empts.

**Operationally:**
1. Write the message from the code, not the intent. If the check is
   `status === "done"`, say *"task X is not done"*, not *"no approved review"*.
2. If the copy must promise more, **strengthen the check** — do not soften the
   honesty. The gap is the defect; wording just hides it.
3. A tool that cannot inspect a whole class must **say so**. `{"locks": []}` must
   mean "I looked everywhere and found nothing"; if a class was not examined, name
   it.
4. Never report success for an unapplied change. "Queued" is success only if
   queueing was the ask.
5. **Review prompt:** for any user-visible message ask *"what would have to be
   true for this sentence to be a lie?"* and check whether the code rules it out.

**Mechanized / applied instances:**
- #506 — the deploy verdict cannot read "verified" on a chiefd `/proc/<pid>/exe`
  binary check alone when a TS pane is stale; the verdict states the actual
  runtime-freshness result and NAMES the stale pane.
- #483 — the commit-namespace convention + lint: a bare `#N` no longer silently
  claims a GitHub-issue guarantee across three overlapping namespaces.
- #530 — operator-escalation delivery states the actual allowlist result and
  fail-closes rather than implying a broadcast guarantee it did not provide.

## Status
Both rules are review checklists first (the general case is semantic and resists a
clean lint). The specific, recurring instances are mechanized above; new instances
should add a guard here rather than only a warning in prose.

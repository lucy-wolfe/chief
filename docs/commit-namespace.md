# Commit-message issue namespaces (#483)

Commit messages in this repo cite bare `#N` references drawn from **three
unrelated numbering namespaces** whose ranges overlap, so a bare `#437` reads as
whichever tracker the reader happens to have open. This has already manufactured
false readings — see #483 for the 2026-07-24 near-miss (`od:idle-cpu #437`
grepping identically to GitHub issue #437) and the six work items that were
reported to the operator against unrelated 2024-era PR numbers.

## The convention

| Namespace | How to write it | What it is |
|---|---|---|
| **GitHub issue** (this repo) | bare `#N` *(historical default)* or `gh#N` | The authoritative board (Project #28). A grep-checkable "did this land?" |
| **Operator-debug series** | `od:#N` | The `od:` scratch series (e.g. `od:idle-cpu`). **MUST** be qualified — a bare `#N` next to `od:` is the exact collision that bit us. |
| **Session task-list card** | never cite to the operator | An agent-side scratchpad the operator cannot open. A tracker nobody can open is not a tracker. Any operator-facing status MUST reference a GitHub issue, never a card number. |

### Rules (from #483, in force)

1. **A GitHub issue reference is a bare `#N` (or `gh#N`).** Bare `#N` has always
   meant "GitHub issue in this repo" here; that stays true. Prefer `gh#N` when a
   message also touches another namespace, so a later grep cannot confuse them.
2. **An operator-debug reference MUST be `od:#N`** — never `od:label #N` with a
   bare number, which is what grep misreads as a GitHub issue.
3. **Never cite a session task-card number to the operator.** Reconcile work onto
   GitHub and report the GitHub number.
4. **Reconcile onto GitHub, never the reverse** — Project #28 is authoritative
   (team-lead decision, 2026-07-24).

## There is no lint for this, deliberately

This convention is carried by review, not by a gate. A hard gate rejecting a
bare `#N` would wedge every commit and require repo-wide buy-in for a new
syntax, and a lint that fires on the common, correct case trains everybody to
ignore it — the "a guard that cries wolf gets switched off" trap this
codebase warns about elsewhere. An earlier advisory script existed and was
removed; nothing replaced it, and that is the settled position rather than a
gap somebody should fill.

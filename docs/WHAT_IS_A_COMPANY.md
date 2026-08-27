# What is a company?

A **company** is a durable AI organization. It can be a trading firm,
engineering group, research desk, agency, or a short-lived contractor group.
chiefd gives that company a clear structure, stable people, and a safe way to
stop and resume work. Where its people are *shown* is a separate question with
two answers today: one tmux session per company for the `chiefd` client, or
panes in a browser. chiefd itself decides only who should be running.

## What you get

- A CEO and recursive departments with named heads and specialists.
- One private Pi history, workspace, inbox, and memory directory per person.
- Placement derived from the organization chart—not ad-hoc panes.
- Explicit skills, tools, model choice, and thinking effort for every hire.
- Durable assignments with acknowledgement, progress, escalation, and result
  delivery instead of relying on chat timing.
- Safe stop, restart, transfer, and removal commands that preserve the
  organization’s disk state until you explicitly remove it.

## The everyday lifecycle

Every verb belongs to the installed `chiefd` binary. The browser flow at
`bun scripts/start-stack.ts` is the other way in; there is no `bun` command
that creates or operates a company.

```bash
# List the companies that exist. Starts nothing.
chiefd

# Create one. Founder asks for a name and a purpose, then boots its CEO.
chief

# Attach to a company, and offer to start it if it is stopped.
chief attach <company>

# Stop compute while keeping people, memory, and sessions on disk.
chief stop <company>

# Return to a CEO-only fresh state. Deletes no durable state.
chief reset <company>

# Remove a company for good. The one verb that deletes durable state.
chief rm <company>
```

Use `chief --help`, or `chiefd <verb> --help`, for the complete human
lifecycle. A company’s files live at `~/.chiefd/orgs/<company>/`, which is the
one canonical home — no flag or environment variable redirects it. The
structural record itself — the organization chart, the roster, who has which
skills and models — is not a file. It is rows in that company's own SQL store,
and **there is no `org.json` and no other JSON projection of it anywhere on
disk**. The only tree a company owns is its people's Pi homes and workspaces;
backing up the directory backs up those and nothing else, because the company
*is* the database. `beacond` is how a caller finds which company's chiefd to
talk to.

## How work happens

Managers delegate before they execute. They choose the smallest capable,
least-cost specialist, give it only the skills it needs, and retain a stronger
reviewer only when the work needs one. Each assignment is acknowledged and
supervised. People write compact, reusable lessons to their own memory after
work settles, so repeatable procedures become faster and more reliable over
time.

The organization chart can grow recursively: a department head can open a
child department or short contract when the parent hierarchy authorizes it.
chiefd records that relationship, and the client places the resulting people
alongside the rest of the company. When work is quiet, unused workers can be
parked without deleting their private history.

## A useful mental model

- **Company**: the durable root organization — its hierarchy, its people and
  its own database.
- **Department**: a durable functional unit inside the company.
- **Contract**: a bounded, transient unit for a specific engagement.
- **Person**: a stable AI hire with a role, model, capabilities, and memory.
- **Operator/head**: a manager who delegates, checks acknowledgements, removes
  blockers, and keeps the organization moving.
- **`org_*` control plane**: the protected no-bash tools materialized
  runtimes use for staffing and unit lifecycle. They are tools, not a CLI, and
  not a second human-facing name for the company.

For the implementation and durability model, read
[Architecture](ARCHITECTURE.md).

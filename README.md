<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
  <img src="docs/assets/logo-light.svg" alt="Chief" width="96" height="96">
</picture>

# chief

**Run a company of AI agents in your terminal.**

[![Release](https://img.shields.io/github/v/release/tribes-protocol/chief?label=release)](https://github.com/tribes-protocol/chief/releases/latest)
[![CI](https://github.com/tribes-protocol/chief/actions/workflows/ci.yml/badge.svg)](https://github.com/tribes-protocol/chief/actions/workflows/ci.yml)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
[![Discussions](https://img.shields.io/github/discussions/tribes-protocol/chief)](https://github.com/tribes-protocol/chief/discussions)

[Quick start](#quick-start) ·
[What is a company](docs/WHAT_IS_A_COMPANY.md) ·
[Architecture](#architecture-in-sixty-seconds) ·
[Examples](examples/) ·
[Contributing](CONTRIBUTING.md)

</div>

![Switching between the rail and department panels](docs/assets/panels.gif)

<sub>The asciinema recording behind that GIF is
[`docs/assets/panels.cast`](docs/assets/panels.cast), and it is a **capture of
a running company** — a real `chiefd`, a real rail, real tmux panes, and real
Pi agents answering for themselves. Nothing in it is drawn by hand. The
company — Northwind Robotics — and everyone in it are fictional.</sub>

## Quick start

You need macOS or Linux, [`tmux`](https://github.com/tmux/tmux), and
[Pi](https://github.com/earendil-works/pi) 0.80.10 or newer
(`npm install -g --ignore-scripts @earendil-works/pi-coding-agent`).

```bash
curl -fsSL https://chief.zipbox.ai/install.sh | sh
export PATH="$HOME/.chief/bin:$PATH"
```

Then found your first company. Any empty directory will do:

```bash
mkdir acme && cd acme && chief
```

That opens **Founder**. Founder learns exactly two things — the company's
**name** and its **purpose** — then creates the company and boots its CEO.
**The CEO is what builds the organisation**; Founder deliberately designs
nothing, and the founding boot is told so in as many words.<!-- pinned by
`the_founding_boot_is_told_to_introduce_itself_and_build_nothing`
(apps/chiefd/crates/chief-cli/src/actuate/spawn_cmd.rs), which asserts the
CEO's first message carries "Create no department, hire nobody" -->
So tell the CEO what you want built — "a three-person research desk that
writes me a daily market brief" — and it creates the departments, appoints
the heads and hires the people. Come back later with `chief` in the same
directory.

`chief ls` lists every company on the box; `chief upgrade` installs the latest
release over this one.

## Why chief

Most tools orchestrate agents as a flat pool of workers, or as panes in a
multiplexer that knows a process is running but not who it is. chief runs a
real **company**: a CEO, a recursive tree of departments, and stable people who
each have a name, a job title, a mandate, a private memory, and their own agent
home. You talk to them, and they talk to each other.

Everything durable is a row in that directory's own SQLite database — not a
process, not a scrollback buffer, not a JSON file. A company survives being
closed, moved, and reopened, because **the company is the database**. Nothing on
disk records which window or pane anybody is in; placement is derived from the
org chart on every pass.

And the whole thing is two programs you meet, plus **`beacond`**, a small
discovery daemon the installer puts beside them. **`chiefd`** is the backend: one daemon per
company directory, and it decides *who should be running*. **`chief`** is the
client: it owns tmux and your terminal, and it decides *where they are shown*.
The daemon cannot see a terminal and the client cannot decide policy, so an
observation is always a report from a client and never a second copy of the
truth.

- A CEO and recursive departments, with named heads and specialists.
- One private Pi history, workspace, inbox, and memory directory per person.
- Placement derived from the org chart, never from ad-hoc panes.
- Explicit skills and tools for every hire; models are Pi's own.
- Durable assignments with acknowledgement, progress, escalation, and result
  delivery — instead of relying on chat timing.
- Messages and reminders that survive a restart: a message is how work reaches
  a person, and a reminder is how a person comes back to it.
- Safe stop, restart, transfer, and removal that keep a person's history until
  you explicitly delete it.
- Parking: a quiet person costs no compute and loses nothing.

## Architecture in sixty seconds

A company is a directory. Running `chief` in it starts one `chiefd` for that
directory, which opens `.chief/db/chief.db` and runs a supervisor loop of six
duties — reconcile, health, mailbox wake, deadlines, reminders, memory. That
loop decides which people should be active right now, and publishes the answer.

The client asks for that answer over HTTP, compares it to the tmux session it
can actually see, and applies the difference: it opens panes, moves them, and
closes them. Every person in a pane is a Pi agent with its own home. `beacond`
is a small box-wide registry that answers "which daemon serves this company",
and admits exactly one daemon per company.

![How chief, chiefd, beacond and the Pi panes fit together](docs/assets/architecture.svg)

| Where | What it owns |
| --- | --- |
| `apps/chiefd/crates/chiefd-daemon` | The backend binary — `chiefd`. |
| `apps/chiefd/crates/chiefd-core` | The typed docstore: the manifest and every ledger, as SQL. |
| `apps/chiefd/crates/chiefd-api` | The HTTP surface over that store. |
| `apps/chiefd/crates/chiefd-host` | Everything the backend touches on the machine. It names no tmux. |
| `apps/chiefd/crates/chief-cli` | The operator client — `chief`: tmux, the terminal, and every verb. |
| `apps/chiefd/crates/beacond` | The small no-auth discovery daemon. |
| `apps/web` | The browser client, and a full second host for a person. |
| `packages/chiefing` | The TypeScript client of chiefd and beacond. No business logic above it. |
| `packages/piing` | Pi artifacts: the skills and extensions copied into Pi homes. |

> **Note:** `apps/web`, the browser host, is **not live and currently broken**.
> The terminal client is the product today. The web host stays in the tree
> because the daemon/client split is designed for a second host; see
> [`apps/web/README.md`](apps/web/README.md) for status.

## Examples

Three companies you can copy and found in a minute — see
[`examples/`](examples/).

| Example | The company |
| --- | --- |
| [`trading-desk`](examples/trading-desk/) | **Meridian Desk** — a paper-trading research desk. Research, Execution, and Risk, with Risk reviewing every trade memo. |
| [`growth-studio`](examples/growth-studio/) | **Signal & Co.** — a growth and social agency. Content, Distribution, and a one-person Analytics unit. |
| [`oss-maintainers`](examples/oss-maintainers/) | **Patchwork Labs** — a company that maintains an open-source repo. Triage, Engineering, and Release. |

## Where to read next

| If you want to | Read |
| --- | --- |
| Understand what a company is | [`docs/WHAT_IS_A_COMPANY.md`](docs/WHAT_IS_A_COMPANY.md) |
| Operate one: disk layout, every command, the runtime | [`docs/OPERATING.md`](docs/OPERATING.md) |
| Understand the code | [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) |
| Contribute | [`CONTRIBUTING.md`](CONTRIBUTING.md) |
| Find any document | [`docs/README.md`](docs/README.md) |
| Report a vulnerability | [`SECURITY.md`](SECURITY.md) |

## Licence

Apache-2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Contributions are
accepted under the [Developer Certificate of Origin](CONTRIBUTING.md#developer-certificate-of-origin);
sign your commits with `git commit --signoff`.

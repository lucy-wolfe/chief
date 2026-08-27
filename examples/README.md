# Examples

Three companies you can found in a minute. Each is a directory of three
markdown files and no code: a `README.md` that describes the company, a
`charter.md` that the CEO turns into an org chart, and a
`first-assignments.md` of three tasks to hand the CEO once it is up.

## The flow, once

```bash
cp -r examples/trading-desk ~/desk && cd ~/desk
chief
```

`chief` in a directory with no company opens **Founder**. Founder learns
exactly two things — the company's **name** and its **purpose** — and then
creates the company and boots its CEO. Both are on the first two lines of
`charter.md`, so paste them:

> Found a company called **Meridian Desk**. Its purpose: a paper-trading
> research desk that turns market data into written, reviewable decisions.

Founder hands you to the CEO. **The CEO is what reads the charter** — Founder
deliberately designs nothing. Tell it:

> Read `charter.md` in the company directory and build the organisation it
> describes: create each department, appoint its head, and hire the
> specialists with the mandates written there.

Then open `first-assignments.md` and hand it the first one.

Come back later with `chief` in the same directory. `chief ls` lists every
company on the box.

## The three

| Example | The company | The shape |
| --- | --- | --- |
| [`trading-desk/`](trading-desk/) | **Meridian Desk** — a paper-trading research desk | Research · Execution · Risk |
| [`growth-studio/`](growth-studio/) | **Signal & Co.** — a growth and social agency | Content · Distribution · Analytics |
| [`oss-maintainers/`](oss-maintainers/) | **Patchwork Labs** — maintains an open-source repo | Triage · Engineering · Release |

## Writing your own charter

A charter is a founding brief, not a configuration file. It has one paragraph
of company name and mission, then one section per department: the head's title
and mandate, then each specialist's title and mandate in two to four sentences.
Write the mandate as a standing instruction to a person — what they own, what
they produce, and who they hand it to — because that is what it becomes.

Two things a charter should never contain. **No model or provider names:** an
agent owns its own model, and pinning one in the charter takes a decision away
from the person who has to live with it. **No org-chart shape you are not
prepared to change:** the CEO can create departments, appoint heads, move
people, and shut units down at any time, and a company that never reorganises
is one nobody is reading.

Keep it short. Four departments and a dozen people is a large company; two
departments and five people is a company that gets something done on day one.

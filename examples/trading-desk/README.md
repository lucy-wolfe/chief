# Meridian Desk — a paper-trading research desk

A three-department company that studies markets and writes down what it would
do and why. **Paper only.** It holds no keys, connects to no broker, and moves
no funds. Its product is the written record.

## The org chart

```text
CEO
├── Research
│   ├── Director of Research (head)
│   ├── Macro Analyst
│   └── Equities Analyst
├── Execution
│   ├── Head of Execution (head)
│   ├── Trader — Systematic
│   └── Trader — Discretionary
└── Risk
    └── Chief Risk Officer (head) — reviews every trade memo
```

The shape is the point: Research decides what the desk believes, Execution turns
a belief into a written plan, and Risk can reject any plan. Risk answers to the
CEO, so a disagreement between Risk and Execution reaches you instead of being
settled between them.

## Launch it

```bash
cp -r examples/trading-desk ~/desk && cd ~/desk
chief
```

`chief` opens **Founder**, which learns two things and nothing more — the
company's name and its purpose. Give it both:

> Found a company called **Meridian Desk**. Its purpose: a paper-trading
> research desk that turns market data into written, reviewable decisions.

Founder creates the company and boots its CEO. **The CEO reads the charter**,
not Founder. Tell the CEO:

> Read `charter.md` in the company directory and build the organisation it
> describes: create Research, Execution and Risk, appoint each head, and hire
> the specialists with the mandates written there.

## What to ask it first

Open [`first-assignments.md`](first-assignments.md) and hand the CEO the first
one: tomorrow's market brief. It is the shortest path to seeing work travel
down through a department and come back as one result.

Then try the third one — the risk-limits proposal — because it is the one that
makes two departments disagree in writing.

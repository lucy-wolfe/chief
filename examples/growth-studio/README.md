# Signal & Co. — a growth and social agency

A three-department company that plans, writes and ships a product launch, then
reports honestly on how it went. It works on a fictional product unless you name
a real one.

## The org chart

```text
CEO
├── Content
│   ├── Content Director (head)
│   ├── Copywriter
│   └── Designer
├── Distribution
│   ├── Head of Distribution (head)
│   ├── Channel Specialist — Social
│   └── Channel Specialist — Owned
└── Analytics
    └── Analyst (head) — a one-person department, deliberately
```

Analytics is one person on purpose. A department in this product does not need
staff; it needs a mandate and a head. Making the analyst a department instead of
a member of Content is what stops the people who ran the campaign from also
grading it.

## Launch it

```bash
cp -r examples/growth-studio ~/studio && cd ~/studio
chief
```

`chief` opens **Founder**, which learns two things and nothing more — the
company's name and its purpose. Give it both:

> Found a company called **Signal & Co.** Its purpose: a growth and
> social-media agency that plans, writes and ships a launch, then reports
> honestly on what it did.

Founder creates the company and boots its CEO. **The CEO reads the charter**,
not Founder. Tell the CEO:

> Read `charter.md` in the company directory and build the organisation it
> describes: create Content, Distribution and Analytics, appoint each head, and
> hire the specialists with the mandates written there.

## What to ask it first

Open [`first-assignments.md`](first-assignments.md) and start with the 30-day
calendar. It is the assignment that visibly crosses a department boundary:
Content writes it, Distribution tells them it cannot be shipped that way, and it
comes back changed.

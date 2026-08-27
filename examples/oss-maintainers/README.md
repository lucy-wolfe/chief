# Patchwork Labs — an open-source maintenance company

A three-department company that maintains somebody else's repository: it
triages, proposes fixes, and drafts release notes. It has no write access and
asks for none — everything it produces is a document a human decides to use.

This example is the self-referential one. Point it at this repository and it
will happily triage chief.

## The org chart

```text
CEO
├── Triage
│   ├── Triage Lead (head)
│   └── Issue Analyst
├── Engineering
│   ├── Engineering Lead (head)
│   ├── Engineer — Fixes
│   └── Engineer — Tests and Reproduction
└── Release
    └── Release Manager (head)
```

Two rules in the charter are worth watching because they are refusals: the
Engineering Lead rejects any patch with no test, and the Release Manager will
not write notes for work Engineering never signed. A company where nobody ever
says no is one where the mandates are decoration.

## Launch it

```bash
cp -r examples/oss-maintainers ~/patchwork && cd ~/patchwork
chief
```

`chief` opens **Founder**, which learns two things and nothing more — the
company's name and its purpose. Give it both:

> Found a company called **Patchwork Labs**. Its purpose: an open-source
> maintenance company that triages issues, proposes fixes, and writes release
> notes for a repository its operator names.

Founder creates the company and boots its CEO. **The CEO reads the charter**,
not Founder. Tell the CEO:

> Read `charter.md` in the company directory and build the organisation it
> describes: create Triage, Engineering and Release, appoint each head, and hire
> the specialists with the mandates written there.

## What to ask it first

Open [`first-assignments.md`](first-assignments.md). The first assignment needs
a repository — name one you actually care about, because the output is only as
useful as the repo is real.

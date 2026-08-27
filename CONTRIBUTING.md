# Contributing to chief

Thank you for wanting to work on this. This document is the whole contract:
what to install, what to run, and the conventions that a pull request is
reviewed against.

`AGENTS.md` in the repository root is the same agreement written for the coding
agents that do most of the work here. It is longer and blunter. When the two
disagree, `AGENTS.md` is the one that is kept current.

---

## 1. Set up a clean machine

| Requirement                                | Version                         | Notes                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------ | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| [bun](https://bun.sh)                      | 1.3.10                          | The package manager and the script runner.                                                                                                                                                                                                                                                                                                                   |
| Rust                                       | pinned by `rust-toolchain.toml` | Do not override it. `rustup` reads the file.                                                                                                                                                                                                                                                                                                                 |
| A native C linker                          | any                             | `cc`/`clang`/MSVC. The release script refuses with the exact install command for your platform if one is missing, and never installs one for you.                                                                                                                                                                                                            |
| `tmux`                                     | recent                          | `chief` is a tmux client. Parts of the suite drive a real tmux.                                                                                                                                                                                                                                                                                              |
| [Pi](https://github.com/earendil-works/pi) | 0.80.10 or newer                | The agent runtime every person in a company runs. The number is a **floor**, never a pin — anything newer passes. It is declared once, in `apps/chiefd/crates/host-primitives/src/pi_floor.rs`, and `scripts/test/pi-floor-single-definition.test.mjs` holds this row to it. Install with `npm install -g --ignore-scripts @earendil-works/pi-coding-agent`. |
| `procps`                                   | any                             | **Linux only, and nothing installs it for you.** Five `attach::` tests shell out to `/bin/kill` to drive a real terminal resize. Without it they fail with `notify the tmux client: Os { code: 2, kind: NotFound }`, which reads exactly like a product fault and is not one. `apt-get install -y procps`.                                                   |

Then:

```bash
git clone https://github.com/tribes-protocol/chief
cd chief
bun install
bun run release          # builds chief, chiefd and beacond and installs them under ~/.chief
export PATH="$HOME/.chief/bin:$PATH"
```

`bun run release` is incremental — re-run it after every pull. `bun run
release:fast` is the same script with dev-tuned cargo settings: much faster to
build, marginally slower at runtime. **Never ship a binary built that way.**

This is the **contributor** path. A user never clones: they run the installer
in the [README quick start](README.md#quick-start), which unpacks a prebuilt
release, and they stay current with `chief upgrade`. Both paths produce the same
versioned layout under `~/.chief` — see
[`docs/OPERATING.md`](docs/OPERATING.md#install-and-upgrade).

### CI needs no secrets

Every job in `.github/workflows/ci.yml` runs on `ubuntu-latest` with
`permissions: contents: read` and reads no `secrets.*`. A pull request from your
fork therefore runs the same gates as one from a branch here, on the default
read-only token. Nothing is skipped for outside contributors.

---

## 2. The standing check list

Run **all of these** before you open or update a pull request. Each one covers
something none of the others does; this list has been reconstructed twice after
a gap in it let a red through.

```bash
bun run typecheck
bun run test                 # the package unit suites
bun run lint
bun run lint:reactive
bun run knip                 # dead code and dependency drift — a CI gate
bun run test:pre-push-guards # the repo-invariant guards
```

And, **after any edit under `apps/chiefd/` — comment-only edits included**:

```bash
cd apps/chiefd
cargo fmt --all --check
cargo clippy --workspace --all-targets
```

Neither cargo command is in the list above, and both are CI gates. Running the
first list faithfully and skipping these two still fails CI. `cargo fmt` is the
usual offender: deleting a test leaves a stray blank line nothing else notices.

### Reading `test:pre-push-guards`

It runs the repo-invariant `node --test scripts/test/*.test.mjs` guards,
derived at runtime from the directory listing rather than a hand-written list
(`node scripts/guard-count.mjs` prints the real count — never carry a
remembered number). It takes one to two minutes and builds no cargo target.

Some of its subtests fail for reasons about **your machine**, not your change.
Check the failing test's own name against this list before treating it as a
regression:

- `sql-only-state.test.mjs` — one subtest assumes no host-level git identity is
  configured. A box with a global `user.name` / `user.email` fails it.
- `gate-matrix-sequence.test.mjs` — asserts sequencing facts that are only true
  inside `CI=1`.

Do not accept a familiar failure count on trust. The cheap proof costs one
command and no build: check the tree out at a SHA from before your work in a
throwaway worktree, symlink `node_modules` into it, run the same guards there,
and `diff` the sorted `^not ok` lines against your run. An empty diff is
evidence; "it was failing before" from memory is not.

### Three suites `bun run test` does not cover

CI is the only thing that runs `OrganizationToolContract`,
`ReminderDeliveryContract` and `EnforcedGateToolSurfaceContract` — they are
excluded from the ordinary piing shards and run in four dedicated
`toolcontract` lanes, because they boot a real tmux host against freshly built
binaries and take minutes. Run them by path after any change to genesis,
materialization, the launch gate, or the identity/bearer path:

```bash
cd apps/chiefd && cargo build --bins
cd packages/piing && npx vitest run test/toolcontract/
```

**Read the count, not just the failures.** A suite-level `beforeAll` throw
reports as `Tests 33 skipped (33)` with `Test Files 3 failed` — every case
skipped and nothing run. A lane with few tests and no failures has HALTED, not
passed.

### Passing the linter is not passing

After a lint fix that adds an import, **run the test** — do not re-run eslint. A
linter checks the shape of code and cannot check that it works. The sharpest
example this repo has produced: eslint was clean, and the imported symbol was
not a function at runtime.

---

## 3. How a change is shaped

### Plan first

Every significant change starts with `plans/<slug>.md`, written **before**
implementation: a four-to-five-sentence TL;DR, scope, acceptance criteria, and
an implementation checklist. `plans/` is git-ignored — the plan is a LOCAL
working document, never committed — so keep it current as verified facts change
and correct it as you learn. Work in an isolated worktree under
`~/worktrees/<slug>/`.

The plan is not ceremony. It is the local record of why a change is shaped the
way it is, held to as a contract while the work lands.

### Tests pin the RULE

**Unit tests that lock in business logic are non-negotiable.** Every change
ships with tests that pin the rule it implements, not merely that the code runs.
A change whose behaviour no test would catch regressing is not finished.

- Deleting a feature means deleting its tests. Changing one means changing them
  in the same commit.
- Every bug fix adds a regression test for the failure, preserves the existing
  assertions, and never weakens or deletes a test to make a change pass.
- Organization hierarchy, tmux placement, messaging, staffing and transfer
  behaviour are product invariants. Lock each with focused unit tests plus
  simulated tmux coverage before changing it.

### Comments carry the WHY, and TOMBSTONEs stay

This is the repository's strongest convention and the one an outside pull
request is most likely to erode, so it is stated as a requirement:

- A comment explains **why**, not what. The code already says what.
- When something is removed, a **TOMBSTONE** comment explains what was there and
  why it left, so the next person does not helpfully reintroduce it. Several of
  this repo's worst bugs were a deleted mechanism someone put back.
- Cite the evidence: the incident date, the measured number, the issue. "229 ms,
  six refusals, on every single launch" is worth more than "sometimes races".

Do not normalise, shorten, or tidy existing long comments. They are load-bearing.

### Engineering principles

From `AGENTS.md`, and they are enforced in review:

- **Do not preserve backward compatibility.** Remove obsolete paths instead of
  adding compatibility layers, fallbacks, or migrations.
- Choose the simplest implementation that fully meets the current requirement.
  No speculative abstraction, configuration, or indirection.
- Grow the system in layers; never trade a working product for unfinished
  complexity.
- Lean on the dependencies already here before adding one or writing your own.
- Make architectural decisions for the long term. A stopgap meant to be
  replaced later is not accepted.

### Two rules that are corrected most often

Read `AGENTS.md`'s "Organization model" section in full before touching any
authority, placement, or department guard. In short: **the CEO is the only
immovable node**, and **authority over structure is the subtree you head, never
a job title.** There is no role gate anywhere in this product. A guard that
refuses because of where somebody _sits_ is wrong.

---

## 4. Every pull request

- [ ] The standing check list above, in full, plus the two cargo commands if
      you touched `apps/chiefd/`.
- [ ] Tests that pin the rule, not just the code path.
- [ ] `CHANGELOG.md` updated with the delivered behaviour, for any significant
      user-requested change.
- [ ] `DECISIONS.md` gets one concise dated line for any product, UX, security,
      workflow, or architecture choice.
- [ ] Your local plan document is current, with every promised item either
      done or explicitly recorded as blocked.
- [ ] Commit messages follow the repo's namespace convention — see
      [`docs/commit-namespace.md`](docs/commit-namespace.md).

### Developer Certificate of Origin

There is no CLA. Contributions are accepted under the Developer Certificate of
Origin, and the DCO check enforces it on every pull request. Add a
`Signed-off-by` line to each commit — `git commit --signoff` (or `-s`) does it
for you:

```
Signed-off-by: Your Name <you@example.com>
```

By signing off you state that you wrote the contribution, or otherwise have the
right to submit it under the project's licence. Read the DCO at
<https://developercertificate.org/>.

**Inbound equals outbound.** Unless you state otherwise in writing, your
contribution is licensed under Apache-2.0 — the same licence the project ships
under. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

---

## 5. Your first pull request

The path in, easiest first. None of these is a lesser contribution; they are
just the ones where the feedback loop is shortest.

1. **Docs.** Anything under `docs/`, or the README. If something was wrong or
   missing when you read it, that is the report and the fix in one.
2. **An example.** `examples/` is three directories of three markdown files
   each, no code. A fourth company — a support desk, a newsroom, a legal
   review team — is a genuinely useful contribution. Copy
   [`examples/trading-desk/`](examples/trading-desk/) and write your own
   charter.
3. **A guarded code change.** Pick an issue labelled `good first issue`. Every
   one of them names the files involved and the acceptance test, so you can
   tell when you are done. Read the surrounding code and its comments — this
   codebase carries its reasoning in load-bearing comments, and that is the
   memory of why the code is the way it is.

Open a [Discussion](https://github.com/tribes-protocol/chief/discussions) if you
want to check an idea before writing it. Questions belong there, not in issues.

---

## 6. Where to read next

[`docs/README.md`](docs/README.md) is the map of every document in the repo, in
reading order. The short version:

1. [`docs/WHAT_IS_A_COMPANY.md`](docs/WHAT_IS_A_COMPANY.md) — the product in 87 lines.
2. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the source map and the authoritative flow.
3. [`apps/chiefd/README.md`](apps/chiefd/README.md) — crate boundaries, the `test-support` policy, the seam lints.
4. [`docs/ORGANIZATION_ARCHITECTURE.md`](docs/ORGANIZATION_ARCHITECTURE.md) — durability, activity, supervision, security invariants, in depth.
5. [`docs/agent-gotchas.md`](docs/agent-gotchas.md) — the traps, written down as they were hit.

## 7. Reporting a security issue

Do not open a public issue. [`SECURITY.md`](SECURITY.md) has the process.

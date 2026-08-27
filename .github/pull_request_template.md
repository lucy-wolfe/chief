## What this changes

<!-- The delivered behaviour, in a sentence or two. Not the diff — the effect. -->

## Why

<!-- The problem, with evidence. An incident date, a measured number, an issue. -->

Plan: `plans/<slug>.md`
Closes: #

---

## Checklist

Every box is a gate. `CONTRIBUTING.md` explains each one, including which
failures are about your machine rather than your change.

**The standing check list**

- [ ] `bun run typecheck`
- [ ] `bun run test`
- [ ] `bun run lint`
- [ ] `bun run lint:reactive`
- [ ] `bun run knip`
- [ ] `bun run test:pre-push-guards`

**If this touched `apps/chiefd/` at all — comment-only edits included**

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets`

**If this touched genesis, materialization, the launch gate, or the identity/bearer path**

- [ ] `cd apps/chiefd && cargo build --bins && cd packages/piing && npx vitest run test/toolcontract/`
      — and I read the **count**, not just the failures. A halted lane reports as all-skipped.

**Tests**

- [ ] Tests pin the **rule** this change implements, not merely that the code runs.
- [ ] A bug fix carries a regression test for the exact failure.
- [ ] No existing assertion was weakened or deleted to make this pass.

**Record**

- [ ] `CHANGELOG.md` updated with the delivered behaviour.
- [ ] `DECISIONS.md` has one dated line, if this made a product, UX, security,
      workflow, or architecture choice.
- [ ] The plan under `plans/` is current — every promised item done, or
      explicitly recorded as blocked.
- [ ] Comments carry the WHY, and anything removed left a TOMBSTONE.

**Provenance**

- [ ] Every commit is signed off (`git commit --signoff`) — see
      [Developer Certificate of Origin](../CONTRIBUTING.md#developer-certificate-of-origin).
      There is no CLA.

<!--
Contributing from a fork? CI needs no secrets: every job runs on ubuntu-latest
with a read-only token, so your pull request runs the same gates as any other.
Nothing is skipped for outside contributors.
-->

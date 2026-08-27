# Agent gotchas

Hard-won operational lessons for engineers and mergers on this repo. Add one
entry per gotcha, newest at the bottom, with the symptom and the rule.

## One ticket = one branch/PR. Never stack two tickets on a single PR.

**Symptom (2026-07-23, #258/#259):** #258 and #259 were stacked on one branch
(`eng-12/258-change-feed`, PR #271). The merger merged the PR when it carried
only #258's first commit (afd7ebc). The #259 endpoint commit (7ef7f0e) was
pushed to the same branch *after* that merge, so it was orphaned — `main`
silently lacked the `/v1/docs/watch` route while the PR read "MERGED" and the
author's status said "done." Cost: a false done-report and a recovery PR (#273).

**Rules:**
- One ticket, one branch, one PR. If work is genuinely stacked, the second
  ticket branches off the first's branch and gets its *own* PR — never appended
  to a PR that may already be merging.
- **Verify the merged diff, not the PR state.** "state: MERGED" only means *a*
  commit merged. Confirm the actual change landed:
  `git merge-base --is-ancestor <your-sha> origin/main` and a `grep` for the
  symbol you added against `origin/main`. Do this before reporting done.
- Mergers: before landing a stacked/re-pushed branch, confirm the PR's head SHA
  (`gh pr view <n> --json headRefOid`) is the commit you intend to land, and that
  the diff vs current `main` is exactly the ticket's delta — nothing already on
  main re-introduced, nothing outstanding dropped.

## A local `waitFor()` longer than 5s needs an explicit per-test `{ timeout }`.

**Symptom (2026-07-23, #261):** `tests/unreachable-watchdog.test.ts` (a
deterministic source-scan check, not a runtime test) blocked the merge because
~11 `test(...)` blocks in `tests/sse-watcher.test.ts` had a local `waitFor()`
helper with a 15s/25s ceiling and *looked* overridden — they passed
`}, { timeout: 15_000 })` as the test's third argument. But **Bun's `test()`
third argument is a bare number, not an options object** — the `{ timeout }`
object is silently ignored, so every one of those async tests actually ran under
the 5000ms default and raced the harness kill before its own wait could report a
real diagnosis. A genuine failure would surface as an opaque timeout, not the
assertion that broke.

**Rule:** any `test(...)` whose internal wait (a `waitFor`, a floor-timer fire,
a channel-state change, any bounded async loop) can exceed Bun's 5000ms default
MUST declare a timeout **as a bare-number third argument**, comfortably above
that internal wait's own ceiling:

```js
test("name", async () => { /* awaits a 15s waitFor */ }, 20_000);  // ✅ applies
test("name", async () => { ... }, { timeout: 20_000 });            // ❌ no-op, ignored
```

This applies to every SSE/async client test in this repo. The watchdog check is
source-scan and deterministic — it blocks the same way every run, so fix it
before handing a branch to the merger, not after a bounce.

## A deploy script's timing budget must exceed the MEASURED worst case, not a guess.

**Symptom (#266, live 2026-07-22):** a live-restart deploy script (since
removed in the open-source redaction) waited a hard 10s (`seq 1 20` at 0.5s) for the old chiefd process to exit after
`SIGTERM`, then refused the deploy if it hadn't. The real drain measured live
at ~11-18s — routinely longer than the budget — so a perfectly healthy
in-progress shutdown lost the race, the script refused mid-deploy with "did
not exit on SIGTERM", and prod was left down with the old daemon gone and no
new one started.

**Rule:** any bounded wait for a live process/service to reach a state (drain,
boot banner, health check) needs its ceiling set from a MEASURED worst case
with real margin (here: 30s, ~2x the observed ~11-18s), not a round number
picked for looking generous. Prefer a loop that breaks the instant the real
condition is met (`kill -0 ... || break`) over a flat sleep, so a fast case
isn't penalized by a larger ceiling. When you bump a budget like this, say so
in the refusal message too (`"...within 30s"`, not just `"...on SIGTERM"`) so
a future false-refusal is diagnosable from the log alone.

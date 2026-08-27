# Capability losses from deleting the Pi patch

Chief carried a patch against `@earendil-works/pi-coding-agent@0.80.10`. The
operator cleared its removal outright. Four of the seven things it did had their
consumers deleted in #1247; the rest are **lost, not replaced**, and this file is
where each one is recorded so a future symptom has a name to match against.

Completeness was proved, when the patch was deleted, by a fourteen-row commit
table enumerated from `git log --follow` on the patch file — because a
remembered set cannot be checked. That working table is not published; this
document is the surviving record of what it found.

---

## C — the fd probe

**Commit** `f80bbce5c`. **Restored behaviour:** stock Pi appends
`--no-require-git` to every `fd` invocation that searches outside a git repo.

That flag arrived in **fd 8.7.0**. On a host whose `fd`/`fdfind` on PATH is
older, **every file search fails** with fd's own usage error — *"Found argument
'--no-require-git' which wasn't expected"*. Measured before the patch: 14
occurrences across 7 people on live cobalt, whose `fdfind` was 8.6.0.

**Who is exposed:** only hosts with a pre-8.7.0 `fd` already on PATH. Pi
downloads a modern `fd` when it finds none, so a host with no `fd` at all is
safe. This is therefore a *host-dependent* loss, not a fleet-wide one.

**Symptom to match:** find/grep tools returning no matches, with
`wasn't expected` in the tool output.

**Remedy if it bites:** upgrade `fd` on the affected host to ≥ 8.7.0, or remove
the old `fd` from PATH so Pi downloads its own.

`tests/find-fd-no-require-git.test.ts` is deleted with this. It could not be
kept: on a modern-`fd` runner it would pass **vacuously** while the capability
was gone, which is worse than failing.

**The test that covered this went too.** `packages/piing/test/PiStartupNonblocking.test.ts`
asserted against the PATCH FILE'S OWN TEXT — it read the `.patch` and matched
hunks. It covered **four** of these losses at once (E, F, G and H), and it could
not survive the file it read. It is named here rather than only in a commit
because it is the single strongest piece of evidence any of these losses ever
had.

---

## D — identity colour in body text

**Commit** `0ea92679e` (#11, extending #433). **Restored behaviour:** primary
body text renders in Pi's default colour rather than each person's identity
colour.

**Symptom to match:** every agent's prose looks the same in a pane; only the
chrome distinguishes them.

**What survives:** identity colour elsewhere in the UI, and
`organizationPersonAccent`, which is chief's own and untouched.

---

## E — Pi files under `.chief`

**Commit** `c6115ffa3`. Recorded as the abort/queue item.

**What survives, and it is most of it:** the Rust side of this
(`chief-cli/src/paths.rs`, `chiefd-log`) is chief's own code and is untouched.
What the patch contributed was Pi's half of keeping its own files under the
company directory rather than a global home.

**Symptom to match:** Pi writing state outside `.chief/` for a managed person.

---

## F — startup boundary tracing

**Commit** `53037b989`. **Restored behaviour:** Pi emits no trace at the
organization startup boundaries the patch instrumented.

**Symptom to match:** a boot that hangs or misbehaves between process start and
first prompt now has no trace to read; diagnosis falls back to the pane's own
output and chiefd's logs.

This was diagnostics only. Nothing depends on it — which is exactly why it is
easy to lose without noticing until the next boot mystery.

---

## G — the provider refresh blocks CEO input

**Commits** `1bfb52402` and `daefd1eaf`. **Restored behaviour:** stock 0.80.10
`await`s `updateAvailableProviderCount()` during init, so input handling does
not exist until the model-registry call returns.

**Consequence:** when the registry is slow the first prompt is late; when it is
**unreachable**, init stalls for the full network timeout.

**The version split matters and is the reason this is not one loss but two
audiences:**

- **The production fleet is on Pi 0.84.3** (via #1241), where upstream enables
  input handlers *before* the provider await. It keeps that partial mitigation.
- **The toolcontract lanes boot this repo's 0.80.10** and get the original
  behaviour **in full**.

**Symptom to match:** a large or timeout-length gap between startup and
first-prompt readiness, *uniform across runs on the same runner*, with the delay
falling **before** any message activity.

---

## H — the managed-Pi first turn races bootstrap

**Commit** `6b51fa440`. **Restored behaviour:** the first message can be
submitted into an editor whose submit handler is not armed yet, so it is
silently dropped.

**Symptom to match:** the first message was sent but the transcript records **no
`message_start` at all** — no late turn, no error, nothing. Per-run flaky rather
than uniform, and worse when `ensureTool` downloads run slow, because stock
0.80.10 completes those before input handling exists.

**G and H can mask each other** — a slow provider await delays the moment H's
race window closes. On a mixed signature, **check H first**: a missing
`message_start` is decisive for H whatever G added on top.

---

## I — live theme authority

**Commits** `d80f4aaab` and `b14f33764`. **Restored behaviour:** Pi no longer
follows a live browser theme change, and mounted cards do not repaint when the
theme changes mid-session.

**What survives:** the **startup** theme. `spawn_cmd.rs` stamps `COLORFGBG`, so
a pane still comes up matching the operator's theme. What is lost is
re-authority *during* a session.

**Symptom to match:** the operator switches light/dark and existing panes keep
the old palette until they are restarted.

`packages/piing/test/PiLiveThemeAuthority.test.ts` is deleted with this.

**One correction on the record:** the plan first said that test's "imports
resolve to code that will not exist". That is **wrong** and was proved wrong by
grepping the rebuilt tree — `InteractiveThemeController` survives in seven files
and `theme-controller.js` still exists, because the patch *extended* an upstream
class rather than creating one. The consequence is real: post-unpatch the test
fails on **behaviour** (an ordinary red) rather than on **module load** (a loud
collection failure), which is a weaker signal and the reason it is deleted in
the same commit as the patch rather than left to fail.

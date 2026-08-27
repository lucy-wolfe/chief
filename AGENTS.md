Always use ASD-STE100 Simplified Technical English when you talk to me.

## ENGINEERING PRINCIPLES

- When working with worktrees or builds, always place them in `~/worktrees/<slug>/`.

- Do not preserve backward compatibility. Remove obsolete paths instead of
  adding compatibility layers, fallbacks, or migrations.
- Choose the simplest implementation that fully meets the current
  requirements. Avoid speculative abstractions, configuration, and
  indirection.
- Grow the system in layers. Start from the smallest version that works end
  to end, and add each new capability on top of a product that already
  works. Never trade a working product for unfinished complexity.
- Keep components modular and concerns clearly separated.
- Prefer established, well-maintained libraries when they reduce overall
  complexity or improve reliability. Do not reimplement common
  functionality without a clear reason.
- Lean on the dependencies already in the project before writing your own
  implementation or adding packages. Do not assume a library lacks a
  capability without checking its documentation and types.
- Make architectural decisions for the long term. Do not accept a stopgap
  that only works for now and is meant to be replaced later.

## FULL-THROTTLE EXECUTION

When concrete independent packets are queued, use the available engineer seats
in parallel. The architect breaks work into bounded packets, assigns every
active seat, and keeps the integrator folding completed packets into the
canonical branch. Do not serialize independent engineering work through one
seat when more seats can make progress.

- Start as many seats as the queued packets justify, up to the available
  capacity.
- Every running seat must have a concrete packet, owner, scope, and handoff.
- Reassign a completed seat immediately when another concrete packet is queued.
- Release a seat only when its packet is complete or explicitly blocked; never
  leave an idle process running.
- Run independent builds and unit checks in parallel when the active phase
  authorizes them.
- Use normal, unrestricted process priority. Never add `nice` or other
  throttling to an authorized build, test, or engineering command.
- The architect reports active seats, packet ownership, merge state, and
  blockers on each status heartbeat.

## Project

chiefd creates and operates persistent, isolated Pi companies. Bare `chief` is
the one directory-scoped front door: without `.chief/db/chief.db` it opens
Founder, and with that database it starts and attaches the company in the
current directory. `chief ls` is the separate box-wide listing. Each company is
a durable company-shaped organization with a recursive department hierarchy,
stable people, shared services, and durable state below its own `.chief/`.
There is one backend daemon, `chiefd`, which `chief` spawns and never a person;
there is one pre-company identity, Founder.

**`.chief/` is not runtime placement, and nothing on disk is.** The current
directory is the company. `.chief/db/chief.db` holds every durable fact, while
`.chief/agent/`, `.chief/bus/`, and `.chief/logs/` hold Pi artifacts. None of
them records which window or pane a person is in.
`person_activity.last_pane_department_id` was that record and #751-P9 deleted
it — "the client derives the rule from the CURRENT tree; **nothing durable
replaces this**" (`chiefd-core/src/store/activity.rs`) — because a persisted
display answer is durably stale between a reparent and the next activity
mutation. Placement is DERIVED, per pass, by the client
(`chief-cli/src/placement.rs`, from the roster plus the desired set) and the
only live record of it is the tmux pane's own `@organization_person_id` /
`@organization_window_id` tags. Read those, never a file, and never cache them.

**And it flows ONE WAY: the client derives and projects, and never reports
observed runtime state upward.** chiefd holds DESIRED state; what the actuator
SAW in a pane is not a fact chiefd stores, asks for, or has a route to
receive. The one upward route is `POST /v1/org/activity/agent-state`, which is
an AGENT reporting about ITSELF — a different thing from a client reporting
what it observed about somebody else, and the distinction is the whole rule. A
new route that carries what a client saw is wrong by design, whatever it is
called and however reasonable the bug that motivates it.

It is written here because it cannot be pinned by a test today, and that is a
recorded gap rather than an oversight: an assertion over the mounted route
table cannot separate the sanctioned self-report from an observation-report
without a maintained classification list, and a maintained list is the
allowlist that rots. Four observation-conditioned code paths have already been
deleted, so treat this paragraph as the guard until an exact-enumeration
contract pin over the mounted route table exists.

## Organization model — THE CEO IS THE ONLY IMMOVABLE NODE

Operator ruling, 2026-08-13. Read this before you touch any authority,
placement, or department guard. It has been got wrong repeatedly — by coding
agents reasoning from the guards, and by product agents reasoning from a folk
model that no surface ever showed them.

**The CEO is the one exempt person.** It never moves, it never converts into
the head of some other department, and it always heads the root department.
That is the whole exemption.

**Everyone else is fluid.** Any other person — including a Chief of Staff, and
including anyone who merely happens to be homed in the executive root — may be:

- moved to any department,
- converted into the head of a new department, which creates that department
  and makes them its head,
- converted back into a plain member,
- reparented, with any child, to any other department.

**A head may do anything with anyone in its own subtree.** Move them, convert
them into a unit head, convert them back, shut a unit down and keep its people,
reparent a child anywhere inside that subtree. Authority is the subtree, so the
CEO — who heads the root and therefore holds every tree — may act on everyone.
Nothing reaches sideways at a peer or upward at a manager; that is the one
direction the tree forbids, and it is not an exemption but the shape of the
model.

The CEO is the only person nobody may act ON. There is no other protected
person, and no protected REGION — not the executive root, not
`office-of-the-ceo`, not the CEO's home or assigned ancestor chain. A guard that
refuses because of where somebody SITS is wrong; only "is this the CEO?" and
"is this the root department?" are legitimate questions.

Two consequences to state outright, because their absence caused the confusion:

1. **Appointing an existing person as a head MOVES that person.**
   `HeadDecision::AppointExisting` re-points their `department_id` into the
   department they now head. (It re-pointed home AND assigned until #1081
   collapsed that pair into one column; the rule is unchanged.) There is no
   "heads a unit from outside it" — heading a department means living in it.
   Any refusal about an appointment
   must say this, or the caller cannot tell why a structural request failed.
2. **Authority over STRUCTURE is the subtree you head, never the job title.**
   `staffingAuthority` has no role gate; every call site checks scope. A person
   who heads nothing may still create a department beneath themselves and staff
   it. Never tell a caller that a STRUCTURAL tool is "CEO-level" or
   "head-level" — no such gate exists.

   **There are NO exceptions, and there is no role gate anywhere in this
   product.** Three tools were once carved out here for one reason: their
   ROUTES enforced nothing, so the TypeScript kind check was the authorization
   itself rather than a pre-flight in front of one. That premise is gone.
   `org_lifecycle_status` is fenced server-side now — it reaches a board whose
   scope the daemon derives from the caller — so it moved to the subtree
   catalog and its `manager()` check came out. A verb that acts on somebody
   else's session is authorized by the daemon asking whether the caller MANAGES
   the target, which is the same subtree question every other verb asks.

   The other two are DELETED rather than fenced. `org_set_thinking` went with
   provider/model management — an agent's reasoning effort is Pi's own setting
   now, not chief's. `org_maintain_session` went whole on 2026-08-24, with all
   three of its actions, on the operator's ruling; the automatic compaction
   survives it and reaches the same pipeline without a tool.

Do not widen the exemption back to the whole executive root. Do not add a role
gate. A guard that refuses a structural move for anybody but the CEO is wrong.

## Quality

- Start every significant user-requested workstream in an isolated worktree by
  creating `plans/<slug>.md` with a four-to-five-sentence TL;DR, scope,
  acceptance criteria, and an implementation checklist. `plans/` is
  **git-ignored and never committed** — the plan is a LOCAL working document,
  written before implementation begins and kept current as verified facts
  change. Do not hand off until every promised item is completed or explicitly
  recorded as blocked.
- Every code change must add or update unit tests that cover its behavior.
- **Unit tests that lock in business logic are non-negotiable.** Every
  change ships with tests that pin the RULE it implements, not merely that
  the code runs: a change whose behavior no test would catch regressing is
  not finished. Deleting a feature means deleting its tests; changing one
  means changing them in the same commit.
- Treat existing tested behavior as a compatibility contract: every bug fix
  must add a regression test for the failure, preserve relevant existing
  assertions, and never weaken or delete a test merely to make a change pass.
- Organization hierarchy, tmux placement, messaging, staffing, transfer
  behavior AND THE OPERATOR WAKE LEASE are product invariants. Lock each
  invariant with focused unit tests plus simulated tmux coverage before
  changing it.

  **THE WAKE LEASE, because it is the one on this list a reader will mistake
  for an implementation detail.** Operator ruling, 2026-08-20: *"If I tell
  chief to message it, it'll come back up and do the 2min settling. We need it
  to always do that when woken. Message or not. If woken, it needs to wait the
  2 mins."*

  **The QUOTE is what they said; the WINDOW is now FIVE minutes.** A second
  ruling on 2026-08-24 — *"lets bump the 2mins to a 5mins"* — moved
  `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS` to 300s, and this floor reads that
  same constant. One number, deliberately: the wake floor IS the settle window
  measured from the click rather than from the last beat, and the invariant is
  a FLOOR and not a ceiling — a longer window satisfies "it needs to wait the
  2 mins" more strictly, not less. Do not "fix" the arithmetic below by
  splitting the two constants; nobody has asked for a second number.

  A person the operator wakes is not parked, not withdrawn from the launch
  fence, and not stopped for a full `ORGANIZATION_SETTLED_IDLE_STOP_LEASE_MS`
  from the instant of the click — **whether or not any message, goal or mail
  demand exists for them**. The durable record is
  `person_activity.operator_wake_at`, stamped by
  `activity::rows::release_idle_park` and read through
  `activity::operator_wake_lease_active`; four rules respect it (the
  automatic-park gate `settled_idle_stop_lease_expired`, the demand rule
  `fence_still_supplies_demand`, both labelled branches of the converge shrink
  half, and `launch_intent_rows::publish`).

  It is a FLOOR and not a ceiling: work arriving inside the window behaves
  exactly as it does with no lease at all. It is not a pin either: the window
  is closed at both ends, and past it the ordinary settle owns the person again
  with no residue.

  **The OBSERVED window is longer than the constant, and that is not a bug.**
  What a watcher measures is the lease plus the reconcile pass's own sampling
  floor: the lease says when the person MAY be parked, and a pass has to come
  round before anybody acts on it. So a stopwatch reads longer than 300s every
  time. Somebody will eventually "correct" that arithmetic from the other end
  — by shortening the lease so the measured total lands on the constant — and
  that is a change to the operator's ruling wearing a bug fix's clothes. The
  floor is on the LEASE, not on what a stopwatch reads.

  **Why it is an invariant and not a tuning knob.** Every OTHER rule that can
  stop somebody reads the AGENT's own reports, and by those reports a person
  woken thirty seconds ago who beat once and was handed nothing to do is
  indistinguishable from one who finished their work. So each of those rules is
  individually correct and collectively deletes the operator's decision, which
  is exactly how this regressed twice in one day (#1185, #1187, #1190). A
  change to converge or activity that "simplifies" the floor away will look
  locally right and will read, to the person holding the mouse, as the product
  ignoring their click.
- For every significant user-requested change, update `CHANGELOG.md` with the
  delivered behavior.
- When a product, UX, security, workflow, or architecture choice is made,
  append one concise dated line to `DECISIONS.md`.
- **NEVER RUN A CARGO OR TMUX-CAPABLE COMMAND BARE. Wrap it:**

  ```
  bash scripts/with-private-tmux.sh <command...>
  ```

  This box is shared, and a pre-push run once destroyed live tmux sessions
  belonging to several people. `bun run test`, `bun run test:pre-push-guards`
  and `scripts/cargo-test-workspace.sh` already wrap themselves — you do not
  wrap those again. Every OTHER cargo command in this file does need it,
  including the `--lib`/`--bins`/`--tests` runs below, `clippy`, and any bare
  `cargo test` you invent.

  Two things make this necessary, and the second is the one nobody guesses.
  A `kill-server` from a fixture is contained by the private `TMUX_TMPDIR` the
  wrapper mints. But the socket a product path resolves does not have to come
  from a fixture at all: `company.rs::boot_socket`'s TIER 3 is the ambient
  `$TMUX`, which is `<socket_path>,<pid>,<pane>` and whose basename inside
  your own pane is literally `default`. Eight product call sites read it
  through `boot_socket_from_env`, several of which then run destructive verbs.
  So a test run started from inside your terminal can address YOUR server
  while no test anywhere names a socket. The wrapper unsets `TMUX`/`TMUX_PANE`
  as well as minting the namespace, which closes that path.

  If a command genuinely cannot be wrapped, say so and leave it unrun. Do not
  run it bare to find out.
- **STARTING A SINGLETON DOES NOT MAKE IT YOURS — and a RECORDED URL cannot
  tell you whether it is still serving anybody.**

  beacond is box-wide, one process, on a fixed port, and
  `discovery::ensure_running` means **the FIRST process that needs it starts
  it**. So which seat started it is an accident of ordering and confers no
  ownership at all. The instinct "I started it, so I clean it up" — correct for
  almost everything else on a shared box — is exactly inverted here, and acting
  on it disrupts every company registered with it. Before stopping any
  singleton, ask what it is currently serving.

  **THE TRAP, which is the half that actually caught somebody: a stale
  rendezvous plus PORT REUSE makes a dead company answer a health check.**

  Measured 2026-08-27. A seat finishing with its own throwaway company asked
  beacond what it held before stopping it, and found a second row — another
  directory, with a URL. Curling that URL returned a healthy daemon, so the
  seat reported a live company belonging to a stranger and nearly filed it as a
  near-miss with an outage attached.

  **It was answering its own daemon.** The stranger's company had been stopped
  long before; its beacond row and its `daemon.json` still named
  `127.0.0.1:8792` and a pid that no longer existed, and the seat's OWN company
  had since bound that same port. One `ps` on the recorded pid and one check of
  what was listening showed the row was stale and the port had simply been
  reused.

  **So the remedy is NOT "curl the URL".** That is the instrument that lied.
  Check the recorded PID, or ask the product, which already guards this
  precisely: `parseDaemonRendezvous` refuses a rendezvous that does not describe
  the directory it was read for, the daemon start loop refuses to adopt a
  published pid that is not the one it spawned, and `pid_is_live` exists for
  exactly this question. **A person reading a URL out of a registry listing has
  none of those guards, which is the whole gap.**

  The rule and its remedy are unchanged by the correction; the near-miss is
  not. Stopping beacond there would have removed a registry holding a row for a
  STOPPED company — bad, and not the outage the first version of this entry
  claimed. **An entry that overstates its evidence is worse than no entry**,
  because the next reader calibrates on the strength of the claim.

  **And the loose one-liner that produced the wrong count in the same
  cleanup.** A sweep for leftover agents reported five surviving Pi processes;
  they were the VNC and desktop processes, matched on incidental text inside
  their argv. A check against the executable NAME reported zero. Together with
  the port-reuse read above, and with a `diff <(…) <(…)` whose `/dev/fd`
  failure was read as "the files differ", that is one lesson with several
  instances in a single session: **ask the narrower, more literal question.**
  Process substitution is unavailable in some shells here — use real temp files
  and compare those.

- Run the relevant checks before handoff — the whole standing list, by
  name, because the root script table is now short enough to type in full:
  `bun run typecheck`, `bun run test` (the package unit suites), `bun run
  lint`, `bun run lint:reactive`, `bun run knip` (dead code and dependency
  drift — a CI gate since #751/G10 that was never on this list, so the only
  place it ran was CI), AND `bun run test:pre-push-guards` (#973)
  — the last one is easy to forget precisely because it covers none of the
  first four. It runs the repo-invariant `node --test
  scripts/test/*.test.mjs` guards, derived at runtime from the directory
  listing rather than hand-listed (`node scripts/guard-count.mjs` prints
  the real count on demand — never carry a remembered number), with no
  cargo build (~1-2 minutes total). These are CI-wired
  (`ci.yml`/`guard-wiring-manifest.mjs`, which keys them BY FILE — there is
  no `package.json` wrapper per guard, and adding one is not the way to
  wire a new guard) but
  were never part of the standing pre-push checklist — a defect one of
  them alone would catch (e.g. a stale allowlist row a file move orphaned,
  #963) was otherwise invisible until it reached batch assembly and got
  misattributed to an unrelated pin. A correct, CI-wired guard nobody runs
  before pushing produces exactly the same outcome as a broken guard. It
  does NOT cover typecheck, lint, or the package suites — run it in
  addition to those, not instead of them.
  - **Run it under `setsid --wait nohup`, and check the exit code before you
    read the output** (measured by `runtime-blind`, which lost two runs to
    it). In the foreground the run is SIGTERM'd partway through and exits
    144, and because its output is fully buffered nothing has been flushed
    yet — so a killed run and a run that produced nothing look identical. The
    reading, not just the remedy: **a guard run that ends with no output has
    not passed, it has not reported.** 144 is the tell. Silence is never the
    green.
  - **`--wait` IS LOAD-BEARING, and this entry said plain `setsid nohup`
    until it was measured.** `setsid` forks whenever it is already a process
    group leader — which it is, invoked from a shell — so the shell does NOT
    wait for it, and **the exit code you check is `setsid`'s, which is 0
    whatever the run did.** The 144 tell this entry is built on can never
    fire in the form this entry prescribed. Measured, both directions:

    ```
    setsid nohup bash -c 'exit 7'         -> status 0   (the child's 7 is lost)
    setsid --wait nohup bash -c 'exit 7'  -> status 7   (propagated)
    ```

    Plain `nohup` waits too, but does not detach the session, which is what
    the SIGTERM problem above needs. `setsid --wait` is the one form that
    both detaches and reports.
  - **And nothing may be chained after a non-waiting run.** The second
    consequence of the fork is worse than a wrong exit code, because it is
    silent: any command after `setsid nohup …` on the same line runs WHILE
    the run is still going. Measured cost, 2026-08-26: a control run for a
    new guard — edit a file, run the guard, restore the file, all chained —
    reported PASS twice, because the restore beat the guard to the file and
    the guard read the restored copy. The guard was genuinely blind and the
    control said it was fine. **If a command mutates state, run it and read
    the result in separate invocations**, whatever the wrapper.
  - ONE of its subtests can fail for a reason that is about YOUR machine,
    not your change, and is not evidence of a regression:
    `gate-matrix-sequence.test.mjs`, which asserts sequencing facts that are
    only true when actually run inside `CI=1`. Check the failing test's own
    name against this before treating it as a real red.

    This entry used to list a SECOND one — a `sql-only-state.test.mjs`
    subtest that assumed no host-level git identity was configured, and
    failed loud on any box carrying a global `user.name`/`user.email`. That
    guard was fixed: it sets `GIT_CONFIG_GLOBAL: '/dev/null'` for its own
    subprocess now, so the host's identity cannot reach it and the exemption
    has no subject. It is struck rather than kept, by this section's own
    rule: a dead red carried as "known" is how a live one gets waved
    through.
  - Two OTHER subtests used to fail on macOS and no longer do, so do not
    carry them as accepted reds: `gate-preflight` (GNU-only `df -BG
    --output=avail`, now POSIX `df -kP`) and `guard-tree-purity.test.mjs`
    (its executed-count parse read the TAP tail `# tests N`, but Node 26
    defaults to the `spec` reporter whose tail reads `ℹ tests N`, so every
    count read 0 and every arm refused). A clean macOS run is 72/72.
    Reporting a dead red as "known" is how a live one gets waved through.
  - ONE MORE needs a host this repo does not build, and it REFUSES in words
    rather than failing an assertion — read the refusal and you can tell it
    from a real red in one line:
    `tmux-session-name-single-definition.test.mjs` (`CANNOT CHECK: cannot
    enumerate tmux sessions to protect the operator`).

    This entry used to name THREE, and the other two —
    `actuator-restart-live.test.mjs` and
    `chiefd-restart-beacond-port.test.mjs` — no longer exist in
    `scripts/test/`. They are struck, not kept: an entry that teaches an
    agent to accept a red which cannot occur is the
    dead-red-hides-a-live-one failure this whole section is written
    against, committed by the section itself. Identify the survivor by ITS
    OWN REFUSAL TEXT, quoted above, never by a position or a count.
  - **COUNT the total, never carry it.** `node scripts/guard-count.mjs`
    prints it on demand. This entry used to name `83/86` and was already
    wrong when it was written (measured `85/88`, two guards later), which
    is this file breaking its own rule inside the entry that exists to stop
    you trusting remembered numbers. A clean dev box is one short of the
    total, for the refusal above.
    **Do not accept a passing count on trust either** — the whole list
    above exists because a dead red hides a live one. PROVE it the cheap
    way, which costs one command and no build: check the tree out at a SHA
    from before your work in a throwaway worktree, run
    `scripts/link-worktree-node-modules.sh` in it, run the same guards
    there, and `diff` the SORTED `^not ok` lines against your run.
    Empty diff and identical `# pass`/`# fail` counts is evidence; "it was
    failing before" from memory is not. If the refusal above PASSES on the
    old tree, it is a regression and it is yours.
  - **A matching baseline tells you the fault is NOT YOURS. It does not tell
    you what the fault IS, and the two are constantly confused.** An
    identical red on the old tree is consistent with "pre-existing and
    harmless" and equally consistent with "a real bug in something both
    trees SHARE" — and a throwaway worktree shares a great deal: the
    symlinked `node_modules`, every built `dist` inside it, the host
    toolchain. Measured case: `bun run lint` was red on the chiefing and
    piing contract suites with identical file lists and identical 23/76
    counts on untouched `origin/main`, and the conclusion drawn from that —
    "a worktree type-resolution artifact" — was wrong. The real cause was
    `packages/testing/dist/index.js` emitted without `tsc-alias`, still
    exporting `from '@/ChiefdBinary'`: a genuine bug, in the one directory
    both trees pointed at, which is exactly why the comparison agreed.
    **An instrument agreeing with itself is not the same as an instrument
    seeing the subject.** So report a matching baseline as what it is — an
    attribution result — and never as a diagnosis. If you cannot say what
    the red IS, say that, and hand it on rather than filing it under
    "environment".
  - **In a new worktree, run `scripts/link-worktree-node-modules.sh`
    BEFORE you run anything else.** One command, idempotent, safe to run
    again in a worktree that is already correct — run it reflexively. It
    mirrors every `node_modules` the tree needs as a real directory, links
    every entry through to the shared checkout EXCEPT `@chief`, points
    `@chief/*` at your own `packages/*`, and then VERIFIES that no
    `@chief` link still resolves outside your worktree. Skipping it, or
    doing the job by hand and missing a package, costs the rest of this
    entry.
    The reason, which is worth keeping because it stopped two agents
    filing a false product bug: **a worktree that symlinks `node_modules`
    from the shared tree resolves `@chief/*` to THAT TREE'S source, not to
    your own** — and the shared
    checkout is somebody else's working copy, usually at a different commit.
    So a guard that reads your worktree by RELATIVE path and the product by
    PACKAGE name is comparing two different revisions of the repo, and it
    will report the difference as a product fault.
    Measured: `tool-surface-artifact.test.mjs` failed with
    `missing: ['org_resume', 'org_stand_down']` on two separate worktrees,
    which reads as "a hosted CEO is granted tools the host cannot build" —
    an alarming and entirely false claim. The guard imports the tool
    CATALOG as `../packages/piing/src/...` (your worktree, which has the new
    tools) while `apps/web` installs the EXTENSION as
    `@chief/piing/extensions/...` (the shared tree, which did not). The
    same run passed in CI, where a real `bun install` makes both halves the
    same tree.
    Fix the worktree rather than the test — that is what the script at the
    top of this entry does, for every package that has a `node_modules`
    and not just the one you noticed. Doing four of six by hand is a real
    measured failure mode: with `packages/eslinter` and `packages/testing`
    left unmirrored, `bash scripts/typecheck.sh` exits 2 with ~20
    `TS2307: Cannot find module '@typescript-eslint/parser'` errors that
    read like missing types and are a missing directory.
    `tool-surface-artifact.test.mjs` now REFUSES this case by name instead
    of reporting it as a product fault: it prints the tree each half was
    read from and tells you to run the script. That is the shape to copy —
    a guard that can be handed two repos should say so rather than diff
    them.
    The general form, and the reason this belongs beside the entry above:
    **when a guard compares two halves of the repo, check that both halves
    are the same checkout before you believe what it says about either.**
    **And the same mechanism reaches the TOOLCHAIN, where it is far more
    dangerous, because a wrong-version binary does not fail — it answers.**
    A worktree `node_modules` linked back to the shared checkout resolves
    that checkout's INSTALL, so the tool you run is the one the shared tree
    last installed and not the one your lockfile names. A missing module is
    loud. A linter of the wrong major is silent and authoritative: it lints
    every file, reports a precise line and column, and is answering about a
    version your branch does not contain. Measured on the eslint 10 bump:
    `package.json` and `bun.lock` both pinned `10.9.0`, the local binary was
    `9.39.3`, and the two disagree in OPPOSITE directions on the same line —
    eslint 9 calls `Number.parseInt(x, 10)` a redundant radix, eslint 10
    calls `Number.parseInt(x)` a missing one, from a byte-identical
    `radix: ['error','as-needed']`. The local run produced three confident,
    precisely-inverted edits and CI rejected them with the opposite message
    at the same three lines. **`bun install --frozen-lockfile` did not fix
    it and reported "no changes"**, because the link chain already satisfied
    it. **One command catches the whole class: run the tool's own
    `--version` and compare it to the lockfile before you believe its
    output.** A confident measurement from an unverified instrument reads
    exactly like rigour, which is why the remedy is a version check and not
    more care.
  - **A cached build and an uncached build of the same command are not the
    same command**, and that difference reads as an impossible bug. Measured
    on `assert-typecheck-nonvacuous.test.mjs`'s live leg: `bash
    scripts/typecheck.sh` was green run by hand and red run by the guard's
    `execFileSync` of that identical command in that identical tree, which
    sent an agent hunting stdio, cwd, TTY and `TURBO_UI` differences and
    found none. There were none. The by-hand run replayed a turbo cache
    hit; the guard injects a probe file into the project graph, which
    invalidates that cache and forces the real `tsc` legs to run — legs
    that were failing because two packages had no `node_modules` (see
    above). **The "difference that was not found" was that the two runs
    were not doing the same work.** Before you conclude that a subprocess
    behaves differently from your shell, check whether turbo answered one
    of them from cache — `FULL TURBO` or `cache hit, replaying logs` in the
    output means that run proved nothing about the code.
  - **`procps` is a host dependency of the Rust suite and nothing installs
    it.** Five `attach::` tests shell out to `/bin/kill` (`kill -WINCH` at a
    tmux client, to drive a real resize), and on a box without it they fail
    with `notify the tmux client: Os { code: 2, kind: NotFound }` — which
    reads exactly like a product fault in the hook path and is not one.
    `apt-get install -y procps` and they are green.
- **`bun run test` does NOT cover four suites, and CI is the only thing
  that runs them.** `.github/workflows/ci.yml` passes `--exclude=` for
  `OrganizationToolContract`, `ReminderDeliveryContract`,
  `EnforcedGateToolSurfaceContract` and `RendezvousWriterBytesContract` to the
  ordinary piing shards, then runs them in four dedicated `toolcontract`
  lanes. The fourth is there for a reason worth knowing: it is the ONLY place
  a real daemon's own `daemon.json` reaches the real TypeScript parser.
  `--serve-only` returns before the publish latch, so the cheap harness cannot
  produce those bytes at all — which is why that seam went untested until a
  live company crash-looped on it. The exclusion is right — they
  boot a real tmux host against freshly built binaries and take minutes — but
  it means every pre-push check a human runs is green over them. Run them by
  path after any change to genesis, materialization, the launch gate, or the
  identity/bearer path:

  ```
  cd apps/chiefd && cargo build --bins
  cd packages/piing && npx vitest run test/toolcontract/
  ```

  **Read the COUNT, not just the failures.** A suite-level `beforeAll` throw
  reports as `Tests 33 skipped (33)` with `Test Files 3 failed` — every case
  skipped and nothing run — so a lane with few tests and no failures has
  HALTED, not passed. `scripts/test/excluded-suites-are-runnable.test.mjs`
  pins that an excluded file is still named by a lane and named here; it
  cannot pin that you read the number.

- **After a lint fix that adds an import, RUN the test — do not re-run
  eslint.** A linter checks the SHAPE of code and cannot check that it works.
  Fixing two errors in one block took four passes, each revealing the next
  convention, and the last one was `isNullish` imported from the package
  barrel `@chief/chiefing` being **not a function at runtime** — the sibling
  test files import it from `@test/support/Nullish`. Eslint was clean and the
  test threw `isNullish is not a function`. That is the sharpest available
  statement that passing the linter is not passing.

- **After any change under `apps/chiefd/`, also run `cargo fmt --all
  --check` and `cargo clippy --workspace --all-targets -- -D warnings`.**
  Both are CI
  gates and NEITHER is in the standing list above, so running that list
  faithfully still lets a red through — the same shape as the
  `test:pre-push-guards` gap, and it cost four separate pushes a failed CI
  round in one day before being written down. `cargo fmt` is the usual
  offender: deleting a test leaves a stray blank line that nothing else you
  run locally notices.
  - **`-- -D warnings` is part of the command, not decoration.** CI appends
    it, and without it clippy prints its complaints and exits 0 — so the
    command LOOKS like it covered you, which is the same trap as
    `--lib`/`--bins` above. This repo's own `clippy.toml` DENIES methods
    (`std::fs::remove_file`, so filesystem effects go through
    `chiefd_host::files`/the executor), and a `disallowed_methods` hit is
    only a WARNING locally — invisible without the flag, a red CI round
    with it. Measured 2026-08-27 on the `models.json` reconcile (#1306): a
    locally clean clippy, a failed CI, and the diagnosis was one flag.
- **`cargo test -p <crate> --lib` does not run the BIN target's tests, and
  it passes with a big number while never compiling yours.** In `chief-cli`,
  `company.rs`, `listing.rs`, `stop.rs` and `founder.rs` are declared in
  `main.rs` rather than `lib.rs`, so `--lib` runs several hundred tests and
  not one of them is `stop::tests` or `company::tests` — count the NAMES you
  expected, never the total, which is exactly the trap the guard-count note
  above describes. Run **both**:

  ```
  bash scripts/with-private-tmux.sh cargo test -p chief-cli --lib
  bash scripts/with-private-tmux.sh cargo test -p chief-cli --bins
  ```

  Found by `runtime-blind` (fixing `boot_socket`, which lives in
  `company.rs`) and confirmed from the other side by `ci-flake`: the CI
  `cli` shard runs 323 tests TWICE because `main.rs` re-declares the
  modules, which is why three races only ever fire in CI. This is worse than
  the `fmt`/`clippy` gap above, because the command LOOKS like it covered
  you.
- **A new `tests/` file — or a new SQL table — needs `cargo test --workspace
  --tests`. Neither `--lib` nor `--bins` compiles it.** An integration test
  lives outside both targets, so a full green run of the standing list can
  sit on top of one that has never been built. The sharpest instance, and
  the one that cost a red main: **every `CREATE TABLE IF NOT EXISTS` in
  `chiefd-core/src/schema.rs` must have a row in `NATIVE_RELATIONAL_TABLES`
  (`apps/chiefd/crates/chiefd-core/tests/two_implementation_stores.rs`)**,
  which exists so a new native table cannot appear without somebody
  answering "does TypeScript also own this concept, and if so which side is
  authoritative". Nothing on the standing list will tell you that — the
  first thing that says so is CI, and it says it by going red on
  `cargo test --workspace shard (core)`.

  When that shard fails, note that `cargo test --workspace (#857 floors)`
  fails with it. That job asserts no counts and is not a second fault: it is
  a pure collector — the `cargo-test-workspace` job in `ci.yml`, whose whole
  body is "if any shard failed, exit 1". One cause, not two.
- **A HEAD-versus-parent guard proves nothing until the commit EXISTS.**
  Running one before you commit compares the PREVIOUS commit to its own
  parent, and passes on somebody else's work. Measured, 2026-08-19:
  `doc-append-only.test.mjs` was run in a dirty worktree and reported 20
  pass / 0 fail, and the same guard checked out AT the commit
  (`f27f7c9f8`, in a throwaway worktree) failed deterministically, 2 of 20 —
  the entry it flagged was the one sitting uncommitted in the tree the first
  run had just read past. It went to CI on that basis and turned main red.
  This is the `--lib`/`--bins` shape above in different clothes: the command
  looked like it covered you and did not. It was checked the wrong way; it
  did not flake. **Commit first, then run any guard whose subject is the
  DIFF** — `doc-append-only` is the one this bit, and every guard that reads
  `HEAD` against a parent has the same property.

  The violation itself is worth one line, because the instruction that
  caused it sounded harmless: **CHANGELOG.md and DECISIONS.md are
  append-only, so a shipped entry is never reordered, reworded, or
  "cleaned up" — a correction is APPENDED as a new newest entry.** Rewriting
  the first line of an entry that has already landed changes the fingerprint
  the guard exists to protect. And when the violating commit is already
  history and HEAD is clean again, do NOT add a `DOCUMENTED_FIRST_LINE_EDITS`
  entry to quieten it: the arm-and-control subtest deletes satisfied
  exceptions and re-checks real HEAD, an unused exception is itself flagged,
  and silencing a guard about a violation that no longer exists is the worst
  available repair.
- **Verify the FILE, never the sentence about the file.** A completion report
  describes the fix its author INTENDED, which is not necessarily the fix the
  tree carries — measured three times in one day on one branch: a hostname
  sweep that reported completeness while matching only the FQDN form, and a
  floor reported as "derived" twice while the file carried a smaller literal.
  A summary is a statement of intent, and intent is exactly what a review must
  not accept as evidence. So a review triggers on the PUSH, reads the changed
  lines at the pushed SHA, and checks any claim about a guard, a floor, a
  sweep, or a count by running the same instrument the claim cites — the
  grep, the file read, the derived count. The same rule one layer down is why
  every carried `DECISIONS.md` entry names its pinning TEST: a claim verified
  against prose — a comment, a report, a remembered ruling — is not verified,
  and both errors that rule caught were about to ship in exactly those
  clothes.

  **And an ACKNOWLEDGEMENT IS NOT AN ARTIFACT.** The rule above assumes there
  is a fix in a tree somewhere to be mis-described. The sharpest case has
  nothing there at all: a ruling arrives, the reply agrees with it, and from
  that moment the author's own account of the work has the item closed —
  while no file was ever touched. Measured: a one-sentence ruling was relayed,
  acknowledged in a message, and reported complete across three successive
  heads; a search for its text over every branch and reflog
  (`git log --all -S`) returned nothing, because it had never been written.
  Both offered explanations — a message crossing, a commit lost to a rebase —
  were wrong, and the search is what settled it.

  **The smaller the ruling, the more completely acknowledging it feels like
  completing it**, because there is nothing in it big enough to feel like
  work. A one-line comment is exactly the size that gets reported from intent.

  So: **a relayed ruling goes into the tree BEFORE the reply, and the reply
  quotes the file and the line.** Reply-then-implement is what produces this,
  and the gap is invisible precisely because both feel like the same act.

  **And after a merge you did not press, grep `origin/main` for anything you
  pushed near the board going green. The window between a green board and the
  button is where a late push goes to die.** Measured: a ledger entry pushed
  after a board went green, merged from the earlier head, and was reported as
  delivered — three of the four things in that commit had ridden and one had
  not. Nothing in the author's view shows this, because at the moment of the
  press the head was genuinely the head and the board was genuinely green.

  Check it by CONTENT and never by ancestry. `git merge-base --is-ancestor`
  returns NO for every branch commit after a squash merge, so it cannot
  separate "did not land" from "landed normally" — run on that commit it gave
  the right answer for the wrong reason, and would have given the same NO if
  all four pieces had ridden. **A fact that looks like the answer, is true,
  and is about something else** is the same instrument failure as trusting a
  mergeability flag that was true when read.
- **Run the guards that NAME the file you edited, not the guards whose
  SUBJECT you edited. The two sets are not the same.** Measured: clearing a
  dead reference from a documentation file deleted the only line on which a
  word and a number sat together, and a count guard asserted exactly that
  coupling by regex over that one line. **The prose was load-bearing and
  nothing in the sentence announced it.**

  State it as a class, because it is one, and this branch produced three
  distinct instances: an append-only ledger where an entry's first line IS its
  fingerprint; this file, where a struck exemption is a rule an agent will
  otherwise obey; and a triage narrative whose sentences carry counts another
  file must agree with. **Documentation is not a safe place to edit**, and the
  fact that a file has no code in it says nothing about whether a guard reads
  it. Before a docs push, `grep` the guard directory for the path you touched
  and run everything that names it — twenty files and ninety seconds, against
  a CI round.

  **And run the guards AGAIN after writing the CHANGELOG and DECISIONS
  entries. Those are the last files written and therefore the only ones your
  pre-push run never saw — and an entry that describes a pattern the change
  removes will contain that pattern.** This is not specific to one sweep: the
  entry is always written last, and describing what a change removed means
  spelling the removed thing. It happened on the branch that added this rule
  — a redaction guard went red in CI on the very entry announcing that its
  class was at zero. One re-run closes the gap the rule above otherwise
  leaves open.

  A related shape, from the same branch: **a guard whose floor encodes
  yesterday's TREE SIZE fails the day the tree legitimately shrinks, and the
  failure reads as a regression rather than as a stale calibration.** Two of
  them were found one CI round apart, each calibrated against a corpus that
  had since been deleted. Derive the floor from the tree at runtime — a
  tracked-file count, a ratio against the file's own structure — so there is
  no number to go stale and no next reader to instruct.
- Commit each completed significant change locally with an intentional
  message.
- Push every completed change to `main`. Do not hold work locally and do not
  ask first — a commit that is not pushed is work nobody else can see.
- **Cross-platform portability:** every code change must build and behave
  identically on macOS and Linux. Darwin and Linux differ in libc type widths
  (e.g. `mode_t` is `u16` on macOS but `u32` on Linux), so never write code
  that only compiles on one. Before handoff, type-check against both targets
  (native build/test on the host plus a `cargo check`/isolated build against
  the other target) and confirm both pass. A change that only works on the
  author's machine is not done.

// #877's manifest: the closed list of every repo-invariant guard file under
// `scripts/test/*.test.mjs`, and how CI is expected to reach it.
//
// #865 fixed today's instance (wired the seven guards that existed at the
// time into `.github/workflows/ci.yml`'s `repo-guards` job). The active job
// now invokes a derived runner. This manifest still decides which files are
// wired, so a new guard is not silently picked up before its status is
// recorded, and a removed guard cannot leave a live manifest row. This file,
// plus
// `scripts/test/guard-wiring.test.mjs`, fixes the CLASS: nothing stops a
// NINTH guard arriving without a wired step and without anyone noticing,
// because passing tests and a passing `knip`/`typecheck` say nothing about
// whether the guard runs anywhere but a laptop.
//
// KEYED BY GUARD FILE, NOT BY PACKAGE.JSON SCRIPT NAME. It used to be the
// latter, and that key forced 46 one-line `"test:<name>": "node --test
// scripts/test/<name>.test.mjs"` wrappers into the root package.json purely
// so this manifest and ci.yml had a name to say — a guard that manufactured
// the pollution it was policing. Nothing else needed the indirection:
// `scripts/gate-matrix-legs.mjs` (and therefore the whole pre-push corpus)
// already DERIVES the guard list from this directory and runs each file with
// `node --test` directly. CI now does the same, so the file IS the identity,
// end to end, and the root script table is a human surface again.
//
// Every entry is one of two shapes:
//   { status: 'wired' }
//     — the guard file must be found invoked (`node --test
//       scripts/test/<file>`) somewhere in .github/workflows/*.yml. If a
//       wired step is ever deleted, this flips from a passing check to a
//       failing one naming exactly which guard fell out.
//   { status: 'local-only', reason: '<why>' }
//     — deliberately NOT run in CI, with a stated reason instead of an
//       omission. The checker also asserts the OPPOSITE direction: a
//       guard marked local-only that is actually found wired in a
//       workflow is a STALE manifest entry, not a passing check — the
//       manifest must track reality, not just gate it.
//
// The active `.github/workflows/ci.yml` repo-guards job invokes the derived
// runner, which selects every `wired` entry below. Local-only entries remain
// excluded by status, and the symmetric stale-local-only check catches a
// future wiring flip being forgotten.
//
// Adding a new `scripts/test/*.test.mjs` file WITHOUT an entry here fails
// `node --test scripts/test/guard-wiring.test.mjs` immediately, naming the
// file — see that file's own negative self-tests for the demonstrated
// red-then-green proof.
export const GUARD_WIRING_MANIFEST = {
  'ci-pr-scope.test.mjs': { status: 'wired' },
  'ci-guard-shard.test.mjs': { status: 'wired' },
  // Keeps the early Piing shards binary-independent by deriving every test's
  // transitive import closure and checking the matching workflow exclusions.
  'piing-ci-binary-partition.test.mjs': { status: 'wired' },
  // #934: the gate driver's OWN preconditions. Every other guard checks the
  // tree; this one checks the instrument that runs them. Four driver defects
  // made the matrix weaker than CI for nine landings while still reporting a
  // green indistinguishable from a real one.
  'gate-preflight.test.mjs': { status: 'wired' },
  // The append-only files gain DUPLICATE entries by the DEFINED BEHAVIOUR of a
  // three-way merge: the same lines added at different offsets on two branches
  // are two independent additions, so git keeps both and raises no conflict.
  // The standing `zero deletions` check is structurally incapable of seeing it
  // -- it asks whether anything was destroyed, and duplication destroys
  // nothing. Three instances landed in one afternoon, one of them a draft
  // asserting the OPPOSITE of the split that shipped.
  'append-only-duplicates.test.mjs': { status: 'wired' },
  // #941: cache-state (compiled vs cached) emitted and asserted on every
  // shared-CARGO_TARGET_DIR gate run, fail-closed if absent/stale.
  'cargo-cache-state.test.mjs': { status: 'wired' },
  // #941: locks scripts/gate-matrix.sh's build -> assert -> record ->
  // verify -> test:unit ordering, derived from the real file.
  'gate-matrix-sequence.test.mjs': { status: 'wired' },
  'gate-matrix-legs.test.mjs': { status: 'wired' },
  // The guard run cannot reach a tmux server it did not create. The driver
  // above spawned every leg with no `env`, so an unsocketed `tmux` from any
  // leg resolved to `/tmp/tmux-<uid>/default` — one run destroyed live
  // sessions belonging to several people on a shared box.
  'guard-run-tmux-isolation.test.mjs': { status: 'wired' },
  // tonight's #942 misreport: `test:unit` lacking `--continue` let one
  // package's failure SIGINT every sibling package mid-run, and a killed
  // package's teardown noise read exactly like a real assertion failure.
  // Locks the classifier (pass/fail/killed/unreached) gate-matrix.sh now
  // runs against every test:unit package, so the next interruption is
  // reported as what it is instead of misread as a fix not working.
  'turbo-package-completion.test.mjs': { status: 'wired' },
  'cargo-test-floor.test.mjs': { status: 'wired' },
  // #918: build/consumer content-hash agreement for the CI e2e
  // CHIEFD_PREBUILT_BINARY artifact-upload/download boundary.
  'prebuilt-binary-manifest.test.mjs': { status: 'wired' },
  // #914: a build and the process consuming its output must agree on
  // CARGO_TARGET_DIR; unit coverage for the record/verify fingerprint guard.
  'cargo-target-dir-agreement.test.mjs': { status: 'wired' },
  // #939: turbo's strict envMode silently strips any env var a task's own
  // code reads but does not declare -- no allowlist, so the derivation
  // must stay honest going forward rather than the exception rotting.
  'turbo-env-audit.test.mjs': { status: 'wired' },
  // #944: TURBO_FORCE=true (every gate's test:unit leg) makes a permanent
  // cache miss and a perfect cache hit look identical -- this is the
  // instrument that checks the cache key itself, with a real (disposable)
  // turbo cache dir and no force, self-contained so it costs ~1s and no
  // cargo/chiefd dependency.
  'turbo-cache-correctness.test.mjs': { status: 'wired' },
  // #948: locks the one property that makes this workspace immune to a
  // task-output-self-hashing bug #944's dry-run design cannot see --
  // .gitignore excluding .turbo/ and dist/.
  'turbo-gitignore-protection.test.mjs': { status: 'wired' },
  // Reporting a gate is a separate act from running one, and it is the act
  // that gets skipped: three finished runs went unreported in one night.
  'gate-report-block.test.mjs': { status: 'wired' },
  'chiefd-workspace-location.test.mjs': { status: 'wired' },
  // #887: every apps/chiefd/{crates,tests} directory with its own
  // Cargo.toml is a listed workspace member or an explicit exclude.
  'chiefd-workspace-membership.test.mjs': { status: 'wired' },
  // #984: `[workspace.lints.rust] warnings = "deny"` is present in
  // apps/chiefd/Cargo.toml and every member actually inherits it. The whole
  // point of the packet was that the denial live in committed manifest state
  // rather than a RUSTFLAGS export, so the guard checks the manifest — a
  // crate landing without `[lints] workspace = true`, downgrading `warnings`
  // in its own table, or planting `#![allow(warnings)]` at its root is a
  // silent opt-out no build failure would ever report.
  'deny-warnings-lints.test.mjs': { status: 'wired' },
  // The #950 guard that checked org-durable-store.ts's `ChiefdBackend` dialled
  // only registered chiefd routes is gone with the file it observed. Deleting
  // it is the point: a guard whose subject no longer exists passes by seeing
  // nothing, which is the exact failure mode it was written to catch. The
  // property it protected now belongs to packages/chiefing, whose route table
  // is fixture-checked in its own suite.
  // #873: real-resolution dependency-declaration gate. A regex-based
  // prototype existed and was correctly rejected (27 false positives on a
  // clean tree, all indistinguishable from the one real planted finding) --
  // this rebuilds it on ts.resolveModuleName + real AST parsing instead,
  // verified to zero false positives on this repo's actual tree before
  // being wired here at all, per the issue's own explicit gate.
  'dep-declaration.test.mjs': { status: 'wired' },
  // #919: derives a deletion target's real reference set (load-bearing
  // imports vs informational mentions) from the tree itself, so a
  // deletion story's hand-typed "affected tests" table can be checked
  // against ground truth instead of trusted -- #817/#946/#820/#830 each
  // shipped a story whose own list fell short of this.
  'deletion-scope-audit.test.mjs': { status: 'wired' },
  // #952: derives every `set<X>ForTests` test-seam setter repo-wide and
  // flags one whose paired real accessor has zero production importers
  // while a test still injects it -- the exact #947 shape (a green test
  // observing a fake production no longer consults). Built on #919's
  // derive-from-the-tree shape rather than a second hand-typed census.
  'orphaned-fake-detector.test.mjs': { status: 'wired' },
  'knip-workspace-map.test.mjs': { status: 'wired' },
  // #858: process-table search literals must not collide with a sibling
  // package's spawned command identity under file-level parallelism.
  'process-search-namespace.test.mjs': { status: 'wired' },
  // #875: compare the real chiefd Rust store structs to the corresponding
  // chiefing TS interfaces; it is explicitly named in repo-guards below.
  'rust-ts-shape-drift.test.mjs': { status: 'wired' },
  'sql-only-state.test.mjs': { status: 'wired' },
  // The generalisation of the cold-attach defect (99e0a3e69): a program name
  // this product hands to a spawn must be absolute, because the process that
  // RESOLVES has to be the process that RUNS. Three processes each answered
  // "where is Pi?" in their own environment, nothing made them agree, and every
  // company that ever ran shipped a bare name to a tmux server.
  'spawn-program-absolute.test.mjs': { status: 'wired' },
  // The class fix for the guard that was not wired at all. `guard-wiring` above
  // polices `scripts/test/*.test.mjs` and nothing else, which is exactly why
  // `scripts/orphanable-spawner-scan.ts` -- a correct scanner with a correct
  // allowlist, one directory up -- was invisible to it for months: no
  // package.json script, no workflow step, no gate driver, and its only test in
  // the parked `tests/` corpus that runs in no lane. This guard's domain is
  // EVERY runnable file under `scripts/`, and its verdict is invoked,
  // registered with a reason, or failing by name. The sweep that wrote it
  // deleted twenty-seven files in the same state.
  'script-invocation.test.mjs': { status: 'wired' },
  // Arrived with the `.env`-loading fix and with no entry here at all, which
  // is precisely the class #877 exists to catch: the guard was correct, ran on
  // its author's laptop, and gated nothing. It stayed invisible because
  // `test:pre-push-guards` is not in the standing pre-push list either, so the
  // first thing that would have named it was CI. Wired, not local-only: it
  // reads only repo files and asserts an ordering `dev-web.mjs` must keep, so
  // there is nothing about it that needs a laptop.
  'repo-env-load.test.mjs': { status: 'wired' },
  // The scanner itself, ported into the corpus that actually runs. Its first
  // run against the real tree found 13 untriaged detached spawn sites against a
  // register of 5 rows, two of them test-owned `beacond` daemons with no
  // child-side self-kill -- the exact shape #987 measured as an
  // eight-to-twelve-hour orphan on a shared build host. None of that was new;
  // all of it had been unreportable for as long as the scanner was dark.
  'orphanable-spawner.test.mjs': { status: 'wired' },
  // #922, moved out of `scripts/test/statusline-fractional-percentages.sh` --
  // a guard parked INSIDE the guard directory, invisible to the `*.test.mjs`
  // derivation that runs everything else, with a header stating it was
  // deliberately unwired. Its subject is live: `.claude/settings.json` runs
  // `.claude/statusline.sh` on every render in this checkout.
  'statusline-fractional-percentages.test.mjs': { status: 'wired' },
  // #751/P4: the organization tool-contract suite is the only thing in CI
  // that calls a TOOL. This guard keeps it non-skippable, keeps it driving
  // the two tool families both P4 defects hid in, and keeps its daemon a
  // fully-actuating `chiefd run` rather than the `--serve-only` mode in which
  // /v1/org/runtime/launch is unreachable.
  'tool-contract-suite-wiring.test.mjs': { status: 'wired' },
  // The tool surface asserted at the ARTIFACT rather than at the list of names
  // every other suite reads. Two measurements nothing gated on: how many of
  // the tools a launch profile declares the host actually BUILDS, and whether
  // every registered tool's serialized parameters carry a top-level
  // `type: "object"` a strict provider will accept. A host building 7 of 60,
  // and three schemas that kill the whole catalog, both shipped green.
  'tool-surface-artifact.test.mjs': { status: 'wired' },
  // #1004: the error taxonomy. A domain refusal answered 500, or a client
  // refusal set that has drifted from the server's, is invisible to every
  // other check here -- `cargo build` is happy either way, a route test
  // asserting 200 never touches the error path, and a client unit test
  // proves nothing about what chiefd actually sends. Two agents found two
  // instances the same day from two different directions, which is what
  // said there were more; the audit found four whole route families
  // answering HTTP 500 for a runtime-generation fence.
  'refusal-taxonomy.test.mjs': { status: 'wired' },

  // One row read by two processes for one purpose. The daemon filtered the
  // runtime-ownership claim on `status`; the client did not, so a company
  // handed off between tmux sockets was projected onto the socket its own
  // release had just vacated — the shared `default` server. The same guard
  // holds the handoff to releasing and re-claiming as one move, because
  // nothing else re-mints a claim after it and a running company that holds
  // none is what the shadow-fleet refusal exists to prevent.
  'runtime-claim-status-single-reader.test.mjs': { status: 'wired' },
  // #1035: the sibling of refusal-taxonomy, one axis over. That guard asks
  // whether a status is the RIGHT one; this asks whether the client is still
  // listening when it arrives. `bench-convergence-timeout` was a correct,
  // documented 503 that no caller could ever observe, because the route waited
  // 30s and `FetchTransport` aborts at 10s -- so `org_bench`'s
  // `error.status === 503` recovery was dead code from the day it shipped and a
  // committed bench read as an outage. The fix wrote the coupling into the
  // constant's doc comment; a coupling documented in a comment is not enforced.
  'client-observable-wait.test.mjs': { status: 'wired' },
  // #1046: the corpus's missing floor. `conformance_tools.rs` counts how many
  // fixtures are blocked, which is true and useful, but a blocked fixture is
  // allowed to claim anything -- and twice now the corpus filled with claims
  // nothing executed: 74 `tools.launcher_calls` argv lists with no producer,
  // and three reminder fixtures pinning a deleted transport. Neither was a
  // wrong assertion; both were assertions with NO SUBJECT. This refuses a
  // transport claim that no Rust runner replays.
  'conformance-fixture-subject.test.mjs': { status: 'wired' },
  // #976: the general basename-collision guard -- a tests/ twin sharing a
  // wired scripts/test/ guard's basename must fail (this is the class fix
  // for the sql-only-state incident above).
  // The merger's mechanical refusal that no state-moving git verb may
  // resolve inside the operator's checkout. Lived in one seat's session
  // scratchpad for a whole programme, so it protected only the machines
  // that seat had touched; checked in so the next merger inherits it.
  'guard-repo-path.test.mjs': { status: 'wired' },
  'wired-guard-basename-collision.test.mjs': { status: 'wired' },
  // #978's `apps-cli-durable-store-preload-import.test.mjs` entry is GONE
  // (#1035), with the guard file and its ci.yml step. It required an
  // apps/cli/test file that references `tests/setup-durable-store` to
  // statically import it; that preload is deleted (it linked against
  // `apps/cli/src/legacy/foundation/paths`, removed by #751/P0), its
  // successor `packages/testing`'s DocstoreDaemon went with the
  // `docstore-only` mode itself, and the guard's own
  // subject set was derived from root package.json's standalone `bun test
  // apps/cli/test/<Name>.test.ts` scripts -- of which there are now zero, so
  // it had already gone vacuous before its subject was deleted underneath it.
  'stub-import-guard.test.mjs': { status: 'wired' },
  // The shard runner must detect directories and changed file bytes that
  // each selected guard leaves in its working tree. The CI shard and serial
  // paths take the live before/after snapshots; this file proves the snapshot
  // instrument itself against demonstrated dirty and clean fixtures.
  // `stub-import-guard.test.mjs` above wrote its live probe under
  // `apps/cli/src/legacy` with a recursive mkdir and cleaned up only the file,
  // so every run recreated three empty directories inside a package #751/P3
  // deleted -- invisible to `git status --porcelain` (git tracks files, not
  // directories) and reported one run LATER by `no-ts-cli-stub.test.mjs`,
  // which had done nothing wrong. Wired, not local-only: a residue check that
  // only ever runs on a laptop leaves CI reporting the same misattributed
  // failure it always did.
  'guard-tree-purity.test.mjs': { status: 'wired' },
  // #1035 pinned the reporter for ONE nested runner; this pins it for every
  // production `node --test` spawn, and pins that the shard parser still reads
  // the format the spawns ask for. Wired for the same reason `guard-wiring`
  // itself is: a guard against a host-dependent default that only runs on a
  // host with the old default is the gap one level up.
  'node-test-reporter-pinned.test.mjs': { status: 'wired' },
  // #1217: `@types/node` describes the runtime the Pi EXTENSIONS run on -- the
  // operator's node, floored by Pi's own `engines.node` -- and not CI's, which
  // is tooling's node and deliberately ahead. Wired because the pairing is easy
  // to get backwards: tying the types to CI's runner image would let an image
  // bump decide what the shipped extensions' types claim.
  'types-match-extension-runtime.test.mjs': { status: 'wired' },
  'parked-suite-triage.test.mjs': { status: 'wired' },
  // #937: `bun test tests` masked all 347 root-level files behind one bare
  // `Cannot find module '@chief/piing'` resolution error whenever a
  // workspace package was unbuilt -- locks that the workspace-build
  // preflight actually runs before the preload that would otherwise throw
  // it bare.
  'workspace-build-preflight-wiring.test.mjs': { status: 'wired' },
  // #886: apps/cli was checked by neither typecheck leg (proven by
  // injecting a real type error and watching `bash scripts/typecheck.sh`
  // exit 0). Fixed by adding apps/cli to tsconfig.json's references plus a
  // non-vacuity floor on that project graph; this guard is the regression
  // proof + the floor's own tamper test.
  'assert-typecheck-nonvacuous.test.mjs': { status: 'wired' },
  // #848: proves the assertMinimumRealFiles floor actually fires for the
  // surviving plain-config leg (tsconfig.extensions.json), not just that it
  // exists -- team-lead's explicit ruling per the merger's
  // block: wired, not local-only, because a check against silent passing
  // that only runs on a laptop is the same gap one level up.
  'typecheck-nonvacuous.test.mjs': { status: 'wired' },
  // #877's own checker, wired alongside the guards it polices -- named
  // here rather than silently exempted, because a guard against unwired
  // guards being itself unwired is exactly the gap #877 exists to close.
  'guard-wiring.test.mjs': { status: 'wired' },
  // #751/G3: the reverse of the entry above. `guard-wiring.test.mjs` checks
  // manifest -> workflow ("a guard marked wired is really invoked"); nothing
  // checked the other way ("every command a workflow step names really
  // resolves"). `bun run <unknown>` exits non-zero, and so does `node --test
  // <a-file-that-does-not-exist>`, so a step naming a deleted target does not
  // skip -- it kills its job and every step after it. That is exactly what
  // happened to `repo-guards` after 477061fa7 deleted
  // `test:ts-durable-store-route-registration` from package.json without
  // touching ci.yml: ~34 guards silently stopped running for weeks, and only
  // a human reading a red run ever found out. Now that the guard steps name
  // FILES rather than script names, the same guard checks both spellings.
  'workflow-script-resolution.test.mjs': { status: 'wired' },
  // The repo's ONE browser acceptance check must be runnable and must stay
  // one. Its predecessor (`scripts/browser-flow-check.mjs`) drove a deleted
  // app and imported a package this repo does not declare, so it could not be
  // loaded at all — for months, while handoffs kept quoting its last green
  // number. This guard imports the real check, resolves every bare specifier
  // against the declared dependencies, and fails on any `apps/*` path that no
  // longer exists. Pure and cheap: it starts no browser and needs no stack.
  'browser-check-runnable.test.mjs': { status: 'wired' },
  // #751/G10: E9-S6 asked for a ci.yml workflow-shape guard (job set, banner
  // text, no continue-on-error) and it was never built. The only ci.yml-shape
  // assertions in the tree sat in tests/ci-workflow.test.ts, inside the parked
  // `bun test tests` corpus, so they ran in no lane at all -- its own triage
  // row said `keep:active` and status `parked`. This is where they were kept,
  // plus the properties E9-S6 named, plus the no-count-in-a-job-name rule the
  // "seventeen invariants" job title (running forty-two) earned.
  'ci-workflow-shape.test.mjs': { status: 'wired' },
  // #907: the derived-guard-count instrument (scripts/guard-count.mjs) --
  // "how many root guards exist" as a computed fact instead of a remembered
  // number, so nobody has to hand-type a count into a brief or a prompt.
  'guard-count.test.mjs': { status: 'wired' },
  // #838's workspace-state completeness checker (the same completeness
  // discipline #877 proved for guard files, applied one layer up to
  // workspace members) -- wired into repo-guards alongside everything else.
  'ci-workspace-state.test.mjs': { status: 'wired' },
  // #890: CHANGELOG.md/DECISIONS.md are append-mostly across every merge;
  // no gate read either file before this, which is how two merges
  // committed them at zero lines with every other check green.
  'doc-append-only.test.mjs': { status: 'wired' },
  'organization-revision-tripwire.test.mjs': { status: 'wired' },
  // #923: for every vitest config's exclude list, every excluded test must
  // resolve to a CI-wired (or named-exempt) package.json script -- named
  // here alongside the guards it polices, matching #877's own rule that a
  // guard cannot be exempted from the wiring discipline it enforces.
  'vitest-exclude-ci-wiring.test.mjs': { status: 'wired' },
  // A test file CI `--exclude=`s from a shard must still be named by a lane
  // AND named in CLAUDE.md's standing list. Three tool-contract suites were
  // excluded from the piing shards and run only in dedicated lanes, so
  // `bun run test` was green over them and three stacked breakages sat there
  // across three stages. Same rule AGENTS.md already states about guards: one
  // nobody runs before pushing is indistinguishable from a broken one.
  'excluded-suites-are-runnable.test.mjs': { status: 'wired' },
  // #930: the merger's gate matrix is a PREDICTION of ci.yml and must be
  // derived from it, never transcribed from the engineer brief -- locks
  // step order (binary provisioning before the cargo test step) and job
  // dependency (cargo-test-workspace needs: build-chiefd), the two
  // structural halves of the #930 incident a driver can check against.
  'ci-sequence.test.mjs': { status: 'wired' },

  // #973: locks `package.json`'s "guards" script -- the cheap, discoverable
  // pre-push entrypoint into every guard this manifest tracks -- against
  // deletion, renaming, or drifting out of sync with the exact
  // --explicit-shell-gate flags scripts/gate-matrix.sh passes.
  'prepush-guards-script.test.mjs': { status: 'wired' },

  // #877/#960 follow-up: two guard files (this one's own detector siblings)
  // had no package.json script at all -- unreachable by name, by CI or a
  // human, discovered only because the merger derives its run-list from
  // the tree rather than from names. Both are ADVISORY audit tools against
  // the LIVE GitHub tracker (a `gh` network call), not correctness gates over
  // this repo's own tree -- wiring either into CI would make every build
  // depend on GitHub API reachability/auth for a check that reports
  // findings for a human to read, never fails a build. Both already
  // degrade gracefully (their own real-tracker tests skip on a `gh` auth/
  // network failure) -- local-only, run by name, is the honest fit.
  'stranded-branch-audit.test.mjs': { status: 'local-only', reason: 'advisory audit against the live git remote branch list (network-dependent); not a correctness gate over this tree' },
  'suspected-done-issues.test.mjs': { status: 'local-only', reason: 'advisory audit against the live GitHub issue tracker (network-dependent); reports candidates for human review, never fails a build' },
  // #970: derives which real, referenced files sit outside BOTH the real
  // typecheck legs and every package's own test-coverage scope -- the
  // general form of the org-world.ts createCompany gap (#959/#970), caught
  // only because eng-4 happened to execute the harness by hand. A NEW
  // entry in the derived gap fails this guard by name.
  'coverage-scope-gap.test.mjs': { status: 'wired' },
  // The half #970 could not express. `coverage-scope-gap` reports files
  // outside BOTH scopes, so a file INSIDE the test scope can never appear in
  // its answer -- and that is where the live instance was sitting:
  // `apps/web/tsconfig.json` excluded its own `test` directory, so 61 web
  // test files plus their harnesses ran on every `bun run test` and were
  // compiled by no type checker at all. 66 real type errors accumulated
  // behind a green typecheck gate. This guard derives both scopes from the
  // same exported derivations (never a second copy of "what is typechecked"),
  // and proves it can see its own subject against a fixture tree in both
  // directions rather than trusting an empty result.
  'typecheck-scope-gap.test.mjs': { status: 'wired' },
  // #751/R11: the reactive scan's own detector, fixtured. `lint:reactive` runs
  // the scan over THIS tree, so it can only ever report what this tree happens
  // to contain -- it cannot tell a detector that works from one that has gone
  // blind, and a blind scan reports clean and is believed. These fixtures build
  // a throwaway tree containing the shape and assert the real scanner sees it
  // (and, in the negative direction, does not flag a one-shot named callback).
  'reactive-scan-named-rearm.test.mjs': { status: 'wired' },
  // `bun run release` is the one documented install command and the one
  // command nobody runs, because everyone who could already has a working
  // tree. ca2da9b57 deleted apps/api and apps/cli/src/legacy and left three
  // references behind — a script import, a bun.lock workspace entry, a
  // Cargo.lock path package — each fatal from a clean clone and each
  // invisible to every checkout that still carried the deleted files. This
  // guard asserts the general property (a deletion left no dangling
  // reference), derived from the tree, not the three names.
  'release-clean-clone.test.mjs': { status: 'wired' },
  // An INSTALLED release must resolve every `@chief/*` specifier its own
  // extensions import. Nothing tested that before: every existing check runs
  // from a checkout, where `node_modules/@chief/*` workspace links make the
  // question invisible. A release shipped the runtime FILES without the
  // package IDENTITY, and every person in a live company crash-looped with a
  // blank cause until the pane's own stderr was harvested by hand.
  'installed-release-loads-its-extensions.test.mjs': { status: 'wired' },
  // "chiefd must not know about tmux" was a P0 mandate stated as a sentence,
  // and a sentence loses to 105 files. P1 turned it into a mechanical
  // work-list -- a per-FILE violation register whose rows named the packet
  // that would drain them -- and #751/P10 deleted the register once the last
  // row went, because an allowlist can only get less wrong and no allowlist
  // cannot get wrong at all. What is left is unconditional: no tracked `.rs`
  // file under a backend scan root matches /tmux/i (comments included),
  // neither side of the client boundary depends on the other through Cargo,
  // every chiefd crate carrying tmux is a DECLARED client crate, and the scan
  // proves it can still see its own subject -- the last one being #848's
  // lesson re-anchored, since a floor on VIOLATIONS cannot survive the
  // violations reaching zero.
  'backend-tmux-boundary.test.mjs': { status: 'wired' },
  // The other half of the same boundary. `backend-tmux-boundary` proves the
  // two crates share no TYPES; this proves the strings they use instead of
  // types still agree. A deleted route is a clean build and a runtime 404.
  'cli-routes-exist.test.mjs': { status: 'wired' },
  // The guard `chiefd`'s own USAGE already had, applied to every OTHER
  // surface that instructs an operator or a model. A sweep closed 14 sites
  // where the product stated something untrue -- a command that never worked
  // (`chiefd catalog --json`), a whole CLI namespace taught to models that no
  // binary routes, retired product names -- and observed that `USAGE` was the
  // one surface that had NOT rotted, because `chief-cli/src/main.rs`'s own
  // `USAGE` doc asserts exactly this about it. (Named by file and symbol, not
  // by a line range: the range this used to carry, and the `lifecycle.rs` file
  // it named, are both gone.) Nothing guarded skills, tool schemas or docs, which
  // is where all fourteen lived. The verb table is DERIVED from `route()` and
  // `main.rs`, never transcribed, and the sites left to their owners are a
  // dated register with exact counts rather than an allowlist: a registered
  // site that is now clean FAILS, and so does a path that no longer exists.
  'model-facing-copy.test.mjs': { status: 'wired' },
  // The Rust half of a rule TypeScript has had since discovery landed
  // (`chiefing/test/PublicSurface.test.ts`: "6969/DEFAULT_BEACOND_URL is
  // compiled in exactly once"). Rust had drifted to three definitions --
  // beacond's own, and a private const in EACH of chiefd's two discovery
  // halves. `chief-cli/src/discovery.rs`'s `unreachable_beacond_detail` is the
  // operator message written for the day an installed binary disagreed with
  // one of them about which port discovery answers on.
  'beacond-port-single-definition.test.mjs': { status: 'wired' },
  // The daemon rendezvous' reader inventory. A two-name inventory of a
  // three-reader record is how an additive field killed a live company: the
  // unnamed reader was in another language, in a package no Rust change
  // touches. Wired because the doc drifts silently and nothing else notices.
  'rendezvous-reader-inventory.test.mjs': { status: 'wired' },
  // #1281: the version ensure restarts a stale component, and the one thing it
  // may never do is stop a person. Both verbs are spelled `stop` and live in
  // sibling files; the wrong one looks more thorough. Wired because the risk is
  // a future edit, not the current code.
  'version-ensure-uses-the-daemon-only-verb.test.mjs': { status: 'wired' },
  // The extension-load guard: the real release packager stages a tree and the
  // real pi loads its extensions out of it. It needs a built `dist` (the
  // packager refuses without one) and the `pi` devDependency, and the guard
  // shard already provides both — `ci-guard-shards.mjs` runs `turbo run build
  // --filter=./packages/*` before it shards, and `--ignore-scripts` does not
  // skip devDependencies. A lane of its own was written first and deleted:
  // it would have been a second place to keep those prerequisites correct.
  // The installer's exit-code contract: a missing prerequisite may not be
  // reported as success. Runs the script's own bytes against stub `pi`/`npm`,
  // and carries its own control proving the assertions bite.
  'installer-pi-exit-codes.test.mjs': { status: 'wired' },
  'installed-release-extensions-load-under-pi.test.mjs': { status: 'wired' },
  // The minimum Pi version, and the documents that quote it. Same shape as the
  // beacond port above and for the same measured reason: a compiled-in constant
  // with a second copy drifts, and one of this constant's copies is a README,
  // whose reader cannot tell it has drifted.
  'pi-floor-single-definition.test.mjs': { status: 'wired' },
  // The front door is `chief` and the daemon is `chiefd`. They used to be
  // `chiefd` and `chiefd-daemon`, which read backwards -- the `d` suffix means
  // daemon and it was on the program a person types. The rename's one real
  // hazard is that `chiefd` briefly named TWO programs, so a half-updated
  // reference reached a real executable with the wrong job rather than failing.
  // This guard holds the six places a binary name is written down to one
  // answer and fails if a `chiefd` front door is reintroduced.
  'binary-names.test.mjs': { status: 'wired' },
  // #983 closed the last reader of the one-address-per-process pane stamp and
  // then deleted both writers. This guard protects the NAME, in production
  // code AND in production comments: a comment describing a live-looking env
  // contract is what the next implementation gets copied from. Derived from
  // the tree with no exception rows at all -- there is nothing in it that can
  // go stale the way #963's orphaned allowlist row did.
  'no-chiefd-url-stamp.test.mjs': { status: 'wired' },
  // The runtime row's process map holds person -> the actuator's PROCESS
  // HANDLE and was called `panes`. One reader validated it against that name,
  // refused every real payload five ways, and took `org_roster` down for every
  // person in every company. Its neighbour broke identically and FAILED OPEN:
  // every wake decision degraded to `unsafe_projection`, SSE-C2 coalescing was
  // silently dead, and nothing was ever red. The quiet half is why this needs a
  // gate rather than a code review -- no other check in this repo can see it.
  // Derived on both legs: the schema and the two structs are re-read from the
  // tree, so there is no list here to rot.
  'runtime-row-process-handle-naming.test.mjs': { status: 'wired' },
  // #751 takeover: two spellings — `TempDir::keep()` and
  // `Box::leak(Box::new(tempfile::tempdir()))` — switch the destructor off, so
  // the directory is never removed. Together they left 86 directories and 37 MB
  // per `chiefd-api` run, which accumulated until `/tmp` hit 100% and SQLite's
  // ENOSPC got read as `corrupt store: company-db`. Wired rather than
  // local-only because the cost is paid on the SHARED build hosts, where nobody
  // is watching a temp directory, and the guard's own first assertion is that
  // its scan root is non-empty.
  'tempdir-lifetime.test.mjs': { status: 'wired' },
  // P3 of the same plan, and the standing user mandate made mechanical:
  // `apps/cli` does not exist, nothing outside the historical record names its
  // entry points, and no Rust source starts bun as a PROGRAM. It deliberately
  // does not ban spawning Pi — Pi is the agent runtime, its CLI is a Node
  // program, and chiefd opens a Founder session by spawning it directly — nor
  // the word "bun" in an operator-facing refusal, which is an instruction to a
  // human. Two guards left with the package they observed
  // (`cli-build-cache-key`, `apps-cli-durable-store-preload-import`): a guard
  // whose subject no longer exists passes by seeing nothing, which is the exact
  // failure mode it was written to catch.
  'no-ts-cli-stub.test.mjs': { status: 'wired' },
  // The public tree carries no private identifier: no box from the private
  // fleet, no operator home path, no internal fleet domain. WIRED because
  // the failure it catches has already happened once and was invisible to
  // every other gate: a redaction packet swept only the FQDN form of a
  // hostname, reported the sweep complete, and left 74 occurrences of the
  // bare form in the tree -- one of them in a file the packet itself named
  // as generalized. Nothing typechecks, lints or tests a leaked machine
  // name, so a reviewer's grep was the only instrument that existed. This
  // guard IS that grep, run on every commit, with an allowlist of the
  // ruled-on survivors that each carry the ruling that kept them.
  'no-private-identifiers.test.mjs': { status: 'wired' },
  // ONE version of record. The shipped version had three literals that
  // disagreed, and the one everybody called authoritative -- package.json --
  // was the only one nothing read: the release workflow hard-coded its seed
  // and took major.minor from whatever tag existed, while its own comment
  // claimed the workspace version governed them. Changing the declared version
  // changed nothing shipped, silently. WIRED because the pair it protects
  // (package.json and the cargo workspace manifest) genuinely cannot derive
  // from each other -- cargo cannot read package.json -- so it is maintained
  // by hand, which is the drift a guard is for and a comment is not.
  'one-version-of-record.test.mjs': { status: 'wired' },
  // Chief never puppets a live agent: no product path types keystrokes at a
  // pane. An ABSENCE pin -- the rule lived in one doc comment, which is prose
  // a future change reads past, and "deliver it by typing at their terminal"
  // is the reflex fix for every delivery bug. The discriminator is the tmux
  // flag (`-M`/`-H`/`-X` are mouse and copy-mode, everything else is
  // keystrokes), so the guard does not fire on the comments explaining it.
  'no-typing-at-a-live-agent.test.mjs': { status: 'wired' },
  // beacond never expires, sweeps or reaps a registration on a timer. The
  // second ABSENCE pin: a TTL is the reflex fix for the first stale row
  // anybody sees, and it is wrong twice over -- it deletes rows belonging to
  // companies that are slow rather than dead, and it needs a background loop
  // the reactive mandate bans. Banned by SHAPE (periodic driver, elapsed-time
  // comparison, TTL constant), because nobody adding one will call it a
  // sweeper.
  'discovery-has-no-sweeper.test.mjs': { status: 'wired' },
  // #1041: a build script may not delete the directory its own package
  // entrypoints live in. `@chief/testing`'s build opened with `rimraf dist`
  // and then took a full `tsc` compile to put the entrypoint back --
  // measured at ~1.3s of the package being unresolvable, which two agents
  // hit as `Failed to resolve entry for package @chief/testing` and both
  // read as flakiness. The turbo edge was declared and honoured; the window
  // was simply visible to every process turbo's graph does not own.
  'build-entrypoint-deletion.test.mjs': { status: 'wired' },
  // #963's class at the last exemption register that had no stale-row check:
  // the `allowedPaths` option three eslinter rules take, written by hand into
  // each package's eslint.config.mjs. An entry orphaned by a file move does
  // not fail -- it stops matching, so the config states a false fact and
  // re-arms silently the day a file appears at that path again.
  'eslint-allowed-paths-liveness.test.mjs': { status: 'wired' },
  // `bc67fe2d2` moved the company tmux session name to `org-<slug>_` in
  // `chief-cli` and nowhere else. The deploy shell kept probing `org-$SLUG`,
  // which after the move matched NOTHING: `company_pane_snapshot` returned
  // empty for every live company, so the gate protecting Pi's panes across a
  // daemon hand-off compared "" with "" and passed. The reason the change did
  // not propagate is that the convention has always had more than one
  // producer -- a tmux-target one and a document-field one -- so there was no
  // single thing to follow. This guard names each producer and its job, drives
  // the real bash against a stubbed tmux, and fails on a fourth copy in any
  // language.
  // The `kill(pid, 0)` liveness probe, in the ONE place the workspace spells
  // it. It was spelled four times and two of the copies read `EPERM` — a
  // process that EXISTS and merely belongs to another user — as DEATH, in two
  // watchdogs whose answer to owner-death is `std::process::exit(0)`. Wired
  // rather than local-only because a fifth copy is a source edit like any
  // other and the guard is a static scan that costs 200ms; it asserts its own
  // non-vacuity first, because a detector that finds nothing reports "exactly
  // one definition" as green.
  'kill-probe-single-definition.test.mjs': { status: 'wired' },
  // A withdrawn launch intent is a person the operator asked for and is not
  // getting, and on `taperoom-inc` (2026-08-20) 310 of 597 fence deletes said
  // nothing at all -- one of them a wake the operator had made 2.165 seconds
  // earlier. The compiler now refuses a `delete_person_fence` that does not
  // name its verb; this guard is the half the compiler cannot see, over the
  // whole-document publish path and the fence clear. Wired rather than
  // local-only because a new silent deleter is a source edit like any other and
  // the guard is a static scan.
  'launch-intent-withdrawal-is-never-silent.test.mjs': { status: 'wired' },
  'mail-demand-reads-one-table.test.mjs': { status: 'wired' },
  // The settle budget: three durations in Rust that stacked to six minutes
  // against an operator cap of two, plus the footer's hand copies of two of
  // them -- one of which had drifted to half the real value under a comment
  // asserting it had not. The Rust sum is a `const _: () = assert!(...)`; this
  // guard is the half the compiler cannot see, across the TS/Rust seam a Pi
  // extension cannot close with an import.
  'settle-budget-single-definition.test.mjs': { status: 'wired' },
  'tmux-fixture-socket-isolation.test.mjs': { status: 'wired' },
  'tmux-session-name-single-definition.test.mjs': { status: 'wired' },
  // `placement.rs::session_name_for_slug`'s prefix-collision proof rested on
  // "`genesis::slugify` is the ONLY producer". It is not --
  // `chiefd_core::store::organization_spec::slugify` is a second one, and no
  // test anywhere asserted the two agreed. The comment stayed CORRECT while
  // the reason it was correct had already changed, which is how it survived
  // review for years. This guard enumerates every slug producer in the tree by
  // SHAPE rather than by name, states each one's keyspace, and holds the two
  // company-slug producers to one adversarial corpus and one copy of the
  // validator.
  'slug-producers-agree.test.mjs': { status: 'wired' },

  // Agent worktrees live under `.claude/worktrees/<name>/`, each a FULL second
  // checkout of this repo, and five guards walked into them and judged another
  // agent's in-progress branch as this checkout's code — two of them dying on
  // `ENOENT` from a dangling path rather than merely reporting a wrong finding.
  // CI has no worktrees, so CI was green through all of it and the damage was
  // entirely to the LOCAL signal: five unactionable reds on every seat's
  // machine is how a suite stops being read.
  //
  // Wired even though the failure it pins cannot occur on a CI runner, because
  // what this guard actually enforces is that the skip set stays in ONE place
  // (`scripts/tree-walk-lib.mjs`). That IS checkable on a clean tree, and it is
  // the half that decays — a new tree-walking guard written from an old
  // template re-spells its own set, and nobody finds out until it reddens
  // somebody else's board weeks later.
  'tree-walk-excludes-nested-checkouts.test.mjs': { status: 'wired' },

  // A rail pane is minted blank and its pty is CANONICAL until a program takes
  // it, so every call `run_sidebar` makes before `Glass::take` is a white
  // rectangle on the operator's glass AND a window in which a sibling rail's
  // `C-M-r` wake is echoed into their sidebar as a literal `^[^R`. Both were
  // in the screenshot they sent; the pane in it had been booting for 804ms.
  //
  // The fix is an ORDERING, which is the class of rule that regresses in
  // silence: move one `await` above that line and nothing goes red, the pane
  // simply goes white again on a box nobody is watching. Wired because the
  // ordering is a property of the source and is therefore checkable anywhere,
  // including on a runner with no terminal at all.
  'rail-takes-the-glass-first.test.mjs': { status: 'wired' },

  // A tmux command over the control client costs under a millisecond; the same
  // command as a process costs ~25ms. The operator asked why tmux felt slow and
  // was told every per-command spawn caller was test-only — from one grep,
  // which had already missed `attach.rs` minting rails in production. A grep
  // answers for the afternoon it was run; a spawn added next month is invisible
  // again. This closes the set: every production tmux spawn is named with the
  // reason it cannot use the transport, and both directions are checked so a
  // justification cannot outlive the spawn it excused. Wired because it reads
  // the source and needs no tmux to run.
  'tmux-spawn-sites.test.mjs': { status: 'wired' },

  // `cold-start-latency.mjs` refuses to print a number unless it can PROVE the
  // start was cold, and that proof had quietly stopped asking anything: it
  // asserted the absence of `~/.chiefd/orgs/…` paths and an `org-<slug>_`
  // session, none of which can be non-empty under any company since a company
  // became a directory. Measured on a fixture with a live daemon, a live
  // actuator, both tmux sessions and a real store, the old proof answered
  // COLD — which is how a regression gets certified by the instrument built to
  // catch it. Every rule now lives in `cold-start-latency-lib.mjs` and is
  // driven here against a WARM fixture that it must refuse. Wired because it
  // reads pure functions and fixture directories, and needs neither tmux nor a
  // company.
  'cold-start-latency.test.mjs': { status: 'wired' },

  // `docs/testing/TEST_SUITE.md` decides whether a live case passes by greping
  // the product's own log strings, which couples the document to those strings
  // — and nothing knew. Two of our own fixes invalidated two of its checks on
  // 2026-08-18: `d8f4e7714` demoted `POST /v1/org/person/wake` to DEBUG, so
  // Case 6's "exactly one signed request" count read 0 before AND after a wake
  // that demonstrably happened (and could not have caught a genuine DOUBLE
  // wake either); `0daa36b0b` deleted `planned=`/`actuated=`, which §4.3 spent
  // a paragraph teaching runners to interpret. Both kept returning a plausible
  // number rather than erroring, which is the hollow-green pattern in the
  // documentation layer. This checks BOTH classes: a string the suite reads
  // must still be emitted, a string a case asserts is gone must stay gone, and
  // a route the suite counts must not have been demoted out of the default log
  // (the level case, which no string search can reach). Wired because it reads
  // only the doc and the source, and needs no box, no tmux and no company.
  'test-suite-log-strings.test.mjs': { status: 'wired' },
}

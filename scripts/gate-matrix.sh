#!/usr/bin/env bash
# #941: THE shared gate driver — debug test build, cargo test, TS test:unit,
# every derived guard, against a shared PERSISTENT CARGO_TARGET_DIR instead
# of a packet-keyed one.
#
# This is the first CHECKED-IN gate driver: previously the merger derived
# each batch driver by `sed` from the previous one, a chain that forked
# before #934's own gate-preflight.sh existed — so that guard, cited in
# receipts and demonstrated red six ways, NEVER RAN in any real gate. This
# file ends that lineage: a guard added to the tree reaches the thing that
# gates, because the thing that gates is checked-in code every seat pulls,
# not a private copy nobody re-derives.
#
# WHY A SHARED TARGET DIR
# ------------------------
# A packet-keyed CARGO_TARGET_DIR means every gate is a 100% cold Rust
# build: ~76% of an ~8m45s gate is Rust, and ~5m43s of that is compilation,
# not test execution (actual `cargo test` runs in ~53s). A packet that
# touches no Rust at all still pays the full bill. Measured saving from a
# shared, persistent target dir: 253s -> 150s worst case (a change to a
# crate everything depends on), ~0s when no Rust changed.
#
# THE TWO CONDITIONS THAT MAKE IT SAFE, NOT JUST FASTER
# --------------------------------------------------------
# A shared dir means batch N+1 can inherit batch N's artifacts — that
# SUCCESSION hazard (not lock contention; the merger's batches are already
# serial) is what these two steps close. Field evidence this is not
# theoretical: on the SAME day, on a per-packet COLD target dir (the
# configuration this packet replaces), a `chiefd` binary was silently
# truncated to 0 bytes and cargo's own fingerprint cache considered it
# up to date and skipped relinking — caught only by a manual sha256 check.
# Cargo's freshness tracking is not a safety net here; #914's content hash
# is.
#
#   (a) Cache state is emitted AND asserted on every run — a cargo leg that
#       may or may not have compiled anything, and does not say which, is a
#       green whose meaning depends on invisible state. scripts/gate-preflight.sh
#       (#934) asserts the stamp scripts/cargo-cache-state.mjs writes is
#       present and fresh from THIS run's own start, fail closed.
#
#   (b) #914's record/verify staleness check runs around the binaries this
#       gate hands to `test:unit`: `record` right after the debug test build,
#       `verify` immediately before `test:unit` consumes what was built —
#       between the build and the first consumer, or it proves nothing.
#
# THE FULL MATRIX, NOT A SUBSET
# -------------------------------
# A driver that runs 6 legs where the merger's matrix proves 30 is #930's
# defect class exactly: a seat adopting it would see green having skipped
# every guard that has caught a real defect. The 22+ scripts/test/*.test.mjs
# guards and every CI-wired shell gate (typecheck.sh, cargo-test-workspace.sh,
# cargo-check-macos.sh — the darwin cross-check) run as part of THIS matrix,
# via scripts/gate-matrix-legs.mjs, which DERIVES that list at runtime from
# guard-count.mjs's own derivation rather than enumerating guard names here.
# A guard added to scripts/test/ or wired into a workflow's run: line
# appears with zero edits to this file — the whole point of retiring the
# derive-by-sed lineage.
#
# CI IS NEVER SELF-SUPPLIED
# ---------------------------
# This driver does NOT `export CI=1`. A driver that supplies the very
# condition its own preflight checks makes that check unfalsifiable in the
# run it is supposed to protect — the merger proved this against its own
# driver: `export CI=1` in the driver meant the CI-unset guard could never
# fire in a real gate, and it sat dead through every batch since. CI is
# read from the CALLER's environment only. Invoke as:
#   CI=1 bash scripts/gate-matrix.sh [repo-root]
# An unset CI refuses at the pre-build preflight arm, loudly, with the
# reason named — that refusal is the guard doing its job, not a bug in this
# driver to route around.
#
# Deliberately OUT OF SCOPE: the fast release profile
# (CARGO_PROFILE_RELEASE_LTO=false / CODEGEN_UNITS=16). It changes
# optimisation, which changes timing, and timing is where the defects this
# gate exists to catch actually live (contention/durability/ordering races).
# A binary with different inlining has different race windows. Do not add it
# here without a separate, explicit decision.
#
# USAGE
#   CI=1 bash scripts/gate-matrix.sh [repo-root]
#   CI=1 GATE_MATRIX_BATCH=69 bash scripts/gate-matrix.sh [repo-root]
# CARGO_TARGET_DIR, if already set in the environment, is honored verbatim
# (so a caller can still force an isolated dir). Unset, this resolves to a
# SHARED, PERSISTENT default — never packet-keyed — so successive gates on
# the same host reuse it.
# GATE_MATRIX_BATCH (#985), if set, names the batch this run is gating —
# the driver pushes `refs/gates/batch-<GATE_MATRIX_BATCH>` at HEAD itself,
# so the gate object is recoverable from a ref rather than from whether a
# person remembered to push one by hand. Unset, the gate still runs and
# reports fully; it just refuses to push a ref rather than guess a batch
# number — see the GATE REPORT's `gate ref:` line for which happened.
set -uo pipefail

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"

# 0) Pre-build preflight arm: authorized build host, CI set (from the CALLER
#    — never supplied by this driver), disk sufficient, the repo's cargo
#    runner present. Cheap; fails fast, before a multi-minute debug build
#    starts. See the "CI IS NEVER SELF-SUPPLIED" note above — this is the
#    check that would have been unfalsifiable had this driver exported CI
#    itself.
#
#    #1041: this runs BEFORE the driver resolves and CREATES
#    CARGO_TARGET_DIR. It used to run after, so a refusal on an unauthorized
#    host had already `mkdir -p`'d /root/cargo-targets-shared on that host
#    while the refusal it then printed says, in its own words, "Nothing was
#    built, installed, or compiled." A refusal that has already written to
#    the machine it is refusing to touch is a refusal whose message is
#    false, and the host check is precisely the one that must cost nothing.
if ! bash "$ROOT/scripts/gate-preflight.sh" "$ROOT" pre; then
  echo "gate-matrix: REFUSED at pre-build preflight — see reason above."
  exit 1
fi

: "${CARGO_TARGET_DIR:=/root/cargo-targets-shared}"
export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

echo "== gate-matrix: CARGO_TARGET_DIR=$CARGO_TARGET_DIR (shared, persistent) =="

rc=0
declare -a RESULTS


# ---- (a) PER-CONSUMER VERIFY. Refuses; does not record a FAIL and continue.
# Proven necessary on 051d6896c: `record` after the build and `verify` before
# test:unit BOTH PASSED, and test:unit then destroyed chiefd itself (writing an
# empty file to the shared CARGO_TARGET_DIR), which the NEXT consumer ate --
# same script PASS at one point in the run and FAIL at another, tree unchanged.
# A single verify proves a property that EXPIRES. Reproduced deterministically:
# chiefd at 0 bytes -> exit 101 "Exec format error"; restored -> exit 0.
# A gate that spots a destroyed artifact and proceeds is reporting on a tree
# that no longer exists, so this REFUSES rather than recording a failure.
verify_artifacts() { # consumer-name
  local who="$1"
  echo "== verify (before $who) =="
  if ! node "$ROOT/scripts/cargo-target-dir-agreement.mjs" verify --root "$ROOT"; then
    echo "REFUSING TO CONTINUE: release artifacts changed since they were recorded, immediately before $who."
    echo "  The gate cannot report on a tree whose build outputs were replaced mid-run."
    exit 1
  fi
}

# #997: a leg that repairs tracked-file state and a leg that verifies it are
# different things, and this gate ran the repair — an UNLOCKED debug build
# that silently rewrote apps/chiefd/Cargo.lock in the working tree every
# batch — before the verify (cargo-test-workspace.sh's --locked leg), which
# then read the just-repaired file and reported green. #996 fixed the
# lockfile; this closes the CLASS: any leg that mutates a tracked file
# between gate start and gate end is refused immediately, naming the exact
# leg that did it, rather than let a later --locked check quietly pass
# against state a later reader will never see in git. Scoped to TRACKED
# files only (`--untracked-files=no`) — a build leg dropping a new file
# under an already-gitignored `target/`/`dist/` is normal and not this
# defect's shape; a TRACKED file changing content or going away is.
assert_clean_tree() { # leg-name
  local leg="$1"
  local dirty
  dirty="$(git -C "$ROOT" status --porcelain --untracked-files=no 2>/dev/null || true)"
  if [ -n "$dirty" ]; then
    echo "REFUSING TO CONTINUE: the working tree has tracked-file changes immediately after \"$leg\"."
    echo "  A gate leg must never mutate a tracked file -- this is exactly the #996/#997 shape:"
    echo "  a repair step running silently ahead of the verify step it invalidates. Committing"
    echo "  this change (or fixing the leg that produced it) is required before this gate can pass."
    echo "$dirty" | sed 's/^/  /'
    exit 1
  fi
}

run() { # name, cmd...
  local name="$1"; shift
  echo "== gate: $name =="
  if "$@"; then RESULTS+=("PASS  $name"); else RESULTS+=("FAIL  $name"); rc=1; fi
}

# 0.25) #987: refuse (never sweep) if a harness/daemon-shaped process is
#       already alive on this host before the gate touches anything. Three
#       real incidents (an 8-12h orphaned beacond, a detached gate-matrix.sh
#       run whose log interleaved into a second, a wedged bare `node --test`
#       holding a FakeRpcChild.mjs harness process) all shared one shape: a
#       stray process can answer a socket or hold a lock a real test expects
#       exclusively its own, and it presents as flakiness, not contamination.
#       This gate does not kill anything — a gate that silently kills
#       processes it did not start is its own hazard. UNEXERCISED (#987
#       written under the no-builds directive) — see the script's own header.
run "pregate-orphan-check (#987)" \
    node "$ROOT/scripts/pregate-orphan-check.mjs"

# 0.5) #993/#994 clean-install smoke: runs BEFORE the debug test
#      build, on purpose — it needs neither the Rust toolchain nor a cargo
#      build to catch its own defect class (preflight's tmux check runs
#      before chiefd is ensured), so a batch that broke `bun install` ->
#      `bun start` fails fast here rather than after paying for a full
#      build first. --mode auto: fast (~11s) on almost every batch, full
#      (~224s cold) only when this commit touches README.md/package.json/
#      any Cargo.toml/scripts/, or once per CARGO_TARGET_DIR session — see
#      the script's own header for the exact trigger list and why.
run "clean-install-smoke (#993/#994, --mode auto)" \
    bash "$ROOT/scripts/clean-install-smoke.sh" --source "$ROOT" --mode auto

# 1) Debug test build, cache-state emitted as part of the same step.
#    #997: --locked, so this leg is not ENTITLED to mutate Cargo.lock.
#    #996's own incident is what this closes: an unlocked build here
#    silently rewrote a stale committed lockfile in the working tree, every
#    batch, and the later --locked cargo-test-workspace.sh leg then read
#    that repaired-but-uncommitted file and passed -- fifteen consecutive
#    green batches against a lockfile that would refuse a real --locked
#    consumer (`bun run release` itself) every time. With --locked here, a
#    future drift between Cargo.toml and Cargo.lock fails on THIS leg,
#    loudly, instead of being silently absorbed and re-discovered later.
GATE_START_MS="$(($(date +%s%N)/1000000))"
#
#    #1041: --bin chiefd is BUILT AND PROVISIONED here, alongside the
#    other two. It was missing, and its absence made this driver's `test:unit`
#    leg strictly WEAKER than the CI job it exists to reproduce: ci.yml's
#    build-chiefd job builds all THREE (`--bin chief --bin chiefd
#    --bin beacond`) and its test-unit job chmod +x's all three, because
#    `resolveChiefdDaemonBinary` looks for `chiefd` specifically —
#    `chiefd` is the operator client and `chiefd` is the backend the
#    docstore suites actually boot. Without it, `@chief/testing`'s three
#    Docstore* suites and ten `@chief/chiefing` contract suites die on
#    "chiefd binary not found at apps/chiefd/target/debug/chiefd" —
#    a failure about provisioning wearing a regression's clothing. The gate
#    that is supposed to be at least as strict as CI must provision at least
#    as much as CI; gate-matrix-sequence.test.mjs now DERIVES that set from
#    ci.yml rather than trusting this comment.
run "cargo build debug chief+chiefd+beacond (cache-state emitted)" \
    node "$ROOT/scripts/cargo-cache-state.mjs" build --root "$ROOT" -- \
      build --locked --bin chief --bin chiefd --bin beacond --manifest-path apps/chiefd/Cargo.toml
assert_clean_tree "cargo build debug chief+chiefd+beacond"

# #941(b) / #914: stamp what was just built, keyed to THIS resolved
# CARGO_TARGET_DIR, so `verify` below can prove it is looking at these
# exact bytes, not a stale or truncated binary left over from a different
# packet's succession through the shared dir.
run "record CARGO_TARGET_DIR agreement" \
    node "$ROOT/scripts/cargo-target-dir-agreement.mjs" record --root "$ROOT"
assert_clean_tree "record CARGO_TARGET_DIR agreement"

# 1b) The binaries just built must POSTDATE the Rust sources in this checkout.
#     An advancing mtime proves a rebuild happened, not that the binary carries
#     the change about to be gated -- a rebuild of unchanged source advances it
#     just as well. This gate checks the ORDERING, which is the part a human eye
#     skips, and it is the reason "rebuild debug test binaries AFTER your final
#     source change" keeps having to be said out loud. It ran nowhere until now:
#     nothing in this repo invoked it.
run "binary freshness (built after the sources in this checkout)" \
    bash "$ROOT/scripts/binary-freshness-gate.sh"

# 2) Provision the debug test binaries at the in-repo path ci.yml's test-unit
#    job expects — exactly as its download-artifact + chmod steps do, not a
#    driver-invented path. This must happen BEFORE the post-build preflight
#    arm, whose own binary-presence check requires them already in place.
mkdir -p "$ROOT/apps/chiefd/target/debug"
for bin in chief chiefd beacond; do
  cp "$CARGO_TARGET_DIR/debug/$bin" "$ROOT/apps/chiefd/target/debug/$bin"
  chmod +x "$ROOT/apps/chiefd/target/debug/$bin"
done
export ORG_LAUNCHER_PROVIDER="${ORG_LAUNCHER_PROVIDER:-anthropic}"

# 3) Post-build preflight arm: binaries in-repo and executable, guard-count's
#    [shell-gate] section non-empty, AND — because CARGO_CACHE_STATE_SINCE_MS
#    is set — the cache-state stamp from step 1 is present and postdates
#    this run's own start. Absent/stale = refuse, exactly like every other
#    precondition here, never a warning.
if ! CARGO_CACHE_STATE_SINCE_MS="$GATE_START_MS" bash "$ROOT/scripts/gate-preflight.sh" "$ROOT" post; then
  echo "gate-matrix: REFUSED at post-build preflight — see reason above."
  exit 1
fi

# 4) Workspace TS packages must be built BEFORE any Rust test that shells
#    out to a TS driver runs — chiefd-e2e::supervisor_handoff_byte_identity
#    invokes a TS driver that imports @chief/testing / @chief/chiefing, and
#    an unbuilt dist/
#    fails them with "Cannot find module", indistinguishable at a glance
#    from a real regression. Found live: the first end-to-end run of this
#    driver put this build inside typecheck.sh, which runs AFTER cargo
#    test — both Rust suites failed for a reason that had nothing to do
#    with their own logic.
[ -d node_modules ] || bun install
run "build workspace packages" \
    bun x turbo run build --filter='./packages/*' --output-logs=new-only
assert_clean_tree "build workspace packages"

# 5) Rust test suite — the repo's sanctioned wrapper (no-fail-fast + a floor
#    check), never a bare, hand-rolled cargo invocation. Content-bounded: its
#    own floor check (cargo-test-floor-lib.mjs) reads the "test result:" line
#    and asserts a minimum executed count, not merely that the process exited
#    0 — a truncated run cannot pass by exit code alone.
verify_artifacts "cargo-test-workspace.sh"
run "cargo-test-workspace.sh" \
    bash "$ROOT/scripts/cargo-test-workspace.sh"
assert_clean_tree "cargo-test-workspace.sh"

# 6) TypeScript must parse and typecheck.
run "tsc --noEmit" bash "$ROOT/scripts/typecheck.sh"
assert_clean_tree "tsc --noEmit"

# 6) Lint.
run "lint" bun run lint
assert_clean_tree "lint"

# #941 follow-up (merger): cargo-check-macos.sh becomes an EXPLICIT stage. It
# previously ran only inside the derived corpus; moving the corpus to test.mjs
# only would otherwise have dropped the darwin cross-check entirely -- the
# exact silent-skip this split exists to prevent, committed while preventing it.
run "cargo-check-macos.sh (darwin cross-check)" \
    bash "$ROOT/scripts/cargo-check-macos.sh"
assert_clean_tree "cargo-check-macos.sh (darwin cross-check)"

# #941(b) / #914: verify AGAIN, IMMEDIATELY BEFORE test:unit consumes the
# binaries provisioned in step 2 — between the build and the first
# consumer, which is the entire guard. A verify at the end of the run would
# pass trivially and prove nothing.
verify_artifacts "test:unit"

# 7) The full TS unit matrix. Output is content-bounded through
#    test-result-parse.sh (auto-detects bun vs cargo, reads the actual
#    summary line rather than an exit code or a byte count) after stripping
#    ANSI — vitest/turbo color codes sit BETWEEN a label and the digits that
#    follow it, which silently breaks a raw-byte pattern match even though
#    the text renders correctly in a color terminal (observed live: a
#    per-package grep for "Tests" dropped seven lines whole this way).
#    Deliberately NOT `> >(tee ...)`: process substitution is unavailable in
#    some sandboxed shells here (the exact trap scripts/binary-freshness-gate.sh's
#    own header documents), and its failure would make this leg FAIL for the
#    WRONG reason — the shell feature broke, not test:unit — which is a
#    worse defect than the one this probe exists to catch. Found live on
#    the first end-to-end run: `/dev/fd/63: No such file or directory`,
#    and the reported failure had nothing to do with the tests.
#
#    TURBO_FORCE=true, and this is load-bearing, not decoration (#939 is
#    held pending a fix for the underlying cause): `CI` is NOT in turbo's
#    cache key on canonical today. `chiefdBinaryTestGate` SKIPS 11 tests
#    with `CI` unset and THROWS (runs them) with `CI` set — so a repo
#    directory where anyone previously ran the unit suites with `CI`
#    unset holds a cache entry that a later `CI=1` run can silently REPLAY,
#    serving a green computed under different inputs. Demonstrated live by
#    the merger against its own driver: `@chief/testing` read
#    "26 passed | 11 skipped" under a replayed CI-unset entry and "37
#    passed" once actually executed — same tree, two different answers, the
#    wrong one faster and indistinguishable from success without reading the
#    per-package line. This driver refuses when CI is unset (see the
#    pre-build preflight arm above), so ITS OWN runs always have CI=1 — but
#    it cannot control what a PRIOR run on the same shared, persistent
#    target dir populated the turbo cache with, and #941's whole subject is
#    a persistent directory: the precondition for this exact hazard.
#    TURBO_FORCE bypasses the cache for this leg entirely until #939 lands
#    CI in the hash and the refusal-based guarantee alone is sufficient;
#    remove this force only alongside that fix, not before it.
TEST_UNIT_LOG="$(mktemp)"
trap 'rm -f "$TEST_UNIT_LOG"' EXIT
TURBO_FORCE=true bun run test > "$TEST_UNIT_LOG" 2>&1
TEST_UNIT_STATUS=$?
cat "$TEST_UNIT_LOG"
if [ "$TEST_UNIT_STATUS" -eq 0 ]; then
  RESULTS+=("PASS  bun run test")
else
  RESULTS+=("FAIL  bun run test")
  rc=1
fi
# ANSI-strip, portably. `sed -i -E` is a GNU spelling: BSD `sed -i` takes its
# NEXT ARGUMENT as the backup suffix, so on macOS `-E` becomes the suffix and
# the run leaves a stray `<log>-E` beside the log -- which the next glob over
# this directory then picks up as if it were a real artifact. Verified here.
# `\x1b` is a GNU escape too, so the ESC is written literally via bash
# ANSI-C quoting, and the edit goes through a temp file instead of `-i`.
sed -E 's/'$'\033''\[[0-9;]*[a-zA-Z]//g' "$TEST_UNIT_LOG" > "$TEST_UNIT_LOG.stripped" \
  && mv "$TEST_UNIT_LOG.stripped" "$TEST_UNIT_LOG"
echo "== content-bounded probe: test:unit =="
if grep -qE '^ *[0-9]+ (pass|fail)$|^Ran [0-9]+ tests? across|^test result:' "$TEST_UNIT_LOG"; then
  bash "$ROOT/scripts/test-result-parse.sh" "$TEST_UNIT_LOG" || true
else
  echo "note: test:unit's log did not match a known bun/cargo summary shape (turbo aggregates per-package) — per-package logs are the content-bounded unit, this top-level log is context only."
fi

# #942's misreport: a package killed mid-run (turbo cancelling siblings on a
# failure, pre-`--continue`) produced teardown stderr that read exactly like
# a real assertion failure, and the killed package's own test file never
# even appeared in the log to say otherwise. `--continue` (above) stops the
# collateral kill; this classifies every in-scope package's ACTUAL fate
# (pass / fail / killed / unreached) so the next interruption — from
# whatever cause — is reported as what it is, never silently folded into
# "FAIL bun run test" as though every package failed the same way.
echo "== per-package completion (pass/fail/killed/unreached) =="
if ! node "$ROOT/scripts/turbo-package-completion.mjs" "$TEST_UNIT_LOG" test:unit; then
  RESULTS+=("FAIL  per-package completion (a package failed, was killed, or was never reached — see above)")
  rc=1
fi
assert_clean_tree "bun run test"

# 8) Every derived guard — the 22+ scripts/test/*.test.mjs files plus every
#    CI-wired shell gate (including the darwin cross-check), derived at
#    runtime, never enumerated here. See scripts/gate-matrix-legs.mjs's own
#    header for why an enumerated list here would be the exact defect this
#    packet retires, relocated rather than fixed.
verify_artifacts "derived guard corpus"
if node "$ROOT/scripts/gate-matrix-legs.mjs" --root "$ROOT" \
     --explicit-shell-gate scripts/cargo-test-workspace.sh \
     --explicit-shell-gate scripts/typecheck.sh \
     --explicit-shell-gate scripts/cargo-check-macos.sh \
     --explicit-shell-gate scripts/clean-install-smoke.sh; then
  RESULTS+=("PASS  derived guard corpus (scripts/gate-matrix-legs.mjs)")
else
  RESULTS+=("FAIL  derived guard corpus (scripts/gate-matrix-legs.mjs)")
  rc=1
fi
assert_clean_tree "derived guard corpus"

echo
echo "== gate-matrix summary =="
printf '%s\n' "${RESULTS[@]}"

# ---- REPORT-READY BLOCK ----
#
# Running a gate is one command; reporting it is a SEPARATE act performed later
# by someone who has already moved on to fixing what they saw. That second step
# is the one that gets skipped -- three finished runs went unreported in one
# night, and from outside a finished-and-unreported run and a still-running run
# are the same state. So the report is emitted BY THE RUN rather than composed
# afterwards: reporting becomes a copy, not a composition.
#
# It prints on BOTH paths. A block that only appears on green is a block nobody
# sees at the moment it matters most.
#
# HOST, because a status without one cannot be verified by anyone else -- four
# machines were checked for a run on a fifth. SHA *and whether it matches its
# origin ref*, because a gate report that does not say what it gated is a green
# about an unnamed tree, and a stale pin two commits behind has already been
# caught once. FAILURES BY IDENTITY, because "1 failed" twice running looks
# identical whether or not a different test was substituted -- and this program
# has had a right-package/wrong-stack failure satisfy a prediction that was false.
GATE_SHA=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
GATE_ORIGIN_STATE="no matching origin ref"
for ref in $(git -C "$ROOT" for-each-ref --format='%(refname)' refs/remotes/origin 2>/dev/null); do
  if [ "$(git -C "$ROOT" rev-parse "$ref" 2>/dev/null)" = "$GATE_SHA" ]; then
    GATE_ORIGIN_STATE="matches ${ref#refs/remotes/}"; break
  fi
done
GATE_COUNTS=$(printf '%s\n' "${RESULTS[@]}" | grep -m1 'GATE_MATRIX_GUARD_COUNTS' || true)
[ -n "$GATE_COUNTS" ] || GATE_COUNTS=$(grep -m1 'GATE_MATRIX_GUARD_COUNTS' "$TEST_UNIT_LOG" 2>/dev/null || echo "not emitted")

# #985: a batch's gate object has, until now, been recoverable ONLY when a
# person remembered to hand-push `refs/gates/batch-<n>` and hand-type its
# tree hash into a batch message -- 27 refs existed because someone
# remembered 27 times, and batches 45/47/48/49/51/52 are what forgetting
# cost (their gate objects are unrecoverable). A tree hash survives rebase
# and linearization and is checkable against canonical forever; a commit SHA
# sitting in a discarded worktree is checkable by nobody. Both are now
# emitted BY THE GATE, not composed afterwards by whoever remembers to.
#
# UNEXERCISED: this script cannot verify its own `git push` succeeds without
# a real remote and a real batch to gate -- the push call below has never
# run. Whoever next runs a real gate should confirm the ref actually lands
# on origin, not just that this script exits 0.
#
# ACCEPTED LOSS: every batch before this leg existed, plus batches 45, 47,
# 48, 49, 51 and 52 specifically (hand-push gaps within the remembered
# range), have no recoverable gate object. That history is not
# reconstructable and is recorded here as accepted loss, not silently
# treated as equivalent to the batches this leg covers going forward.
# #989: a gate run is a write intent against canonical, so the lease is
# verified BEFORE the retain-push rather than after. The hazard is a stale seat
# whose first act on waking is to commit and push; a check that runs after the
# push warns about damage instead of preventing it. Advisory here (reported,
# not fatal) only because the lease is opt-in until every merger seat claims
# one -- set CANONICAL_LEASE_REQUIRED=1 to make it fail closed.
GATE_LEASE_STATE="not checked"
if [ -x "$ROOT/scripts/canonical-writer-lease.sh" ]; then
  if GATE_LEASE_OUT=$("$ROOT/scripts/canonical-writer-lease.sh" --verify 2>&1); then
    GATE_LEASE_STATE="$GATE_LEASE_OUT"
  else
    GATE_LEASE_STATE="NOT HELD: $GATE_LEASE_OUT"
    if [ "${CANONICAL_LEASE_REQUIRED:-0}" = "1" ]; then
      echo "REFUSING TO GATE: canonical writer lease not held by this seat." >&2
      echo "  $GATE_LEASE_OUT" >&2
      exit 1
    fi
  fi
fi

GATE_TREE=$(git -C "$ROOT" rev-parse HEAD^{tree} 2>/dev/null || echo unknown)

GATE_BATCH="${GATE_MATRIX_BATCH:-}"
GATE_PUSH_STATE="not attempted"
if [ -z "$GATE_BATCH" ]; then
  GATE_PUSH_STATE="REFUSED: GATE_MATRIX_BATCH not set -- refusing to guess a batch number rather than push an unlabeled or wrongly-labeled ref"
elif [ "$GATE_SHA" = "unknown" ]; then
  GATE_PUSH_STATE="REFUSED: HEAD SHA could not be resolved"
else
  GATE_REF="refs/gates/batch-${GATE_BATCH}"
  if git -C "$ROOT" push origin "${GATE_SHA}:${GATE_REF}" >/dev/null 2>&1; then
    GATE_PUSH_STATE="pushed ${GATE_REF} -> ${GATE_SHA}"
  else
    GATE_PUSH_STATE="FAILED to push ${GATE_REF} -- see gate-matrix's own stderr above for the git error"
  fi
fi

echo
echo "===== GATE REPORT ====="
echo "host:     $(hostname)"
echo "sha:      $GATE_SHA ($GATE_ORIGIN_STATE)"
echo "tree:     $GATE_TREE"
echo "gate ref: $GATE_PUSH_STATE"
echo "lease:    $GATE_LEASE_STATE"
echo "exit:     GATE_MATRIX_EXIT:$rc"
echo "seconds:  ${SECONDS}"
echo "legs:"
printf '%s\n' "${RESULTS[@]}" | sed 's/^/  /'
echo "packages:"
if [ -f "$TEST_UNIT_LOG" ]; then
  sed -r 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$TEST_UNIT_LOG" \
    | grep -E '^[^ ]+:test:unit: +Tests +[0-9]' | sed 's/^/  /' || echo "  (no per-package result lines)"
else
  echo "  (test:unit log absent)"
fi
echo "failures (by identity, never by package):"
if [ "$rc" = "0" ]; then
  echo "  none"
else
  # SCOPED to the FAILING PACKAGE's own output. An unscoped grep over the whole
  # log lifted `at handleAction (...)` frames out of an unrelated PASSING test's
  # log line and presented them as this failure's stack -- a plausible wrong
  # answer, in the field whose entire purpose is identity. Frames are only
  # identity if they belong to the failure.
  {
    plain=$(sed -r 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$TEST_UNIT_LOG" 2>/dev/null || true)
    failed_pkgs=$(printf '%s\n' "$plain" | grep -E '^[^ ]+:test:unit: +Tests +.*[0-9]+ failed' | cut -d: -f1 | sort -u)
    if [ -z "$failed_pkgs" ]; then
      echo "no package reported a failing test count; see the legs below"
    fi
    for pkg in $failed_pkgs; do
      # Only lines this package emitted.
      pkg_lines=$(printf '%s\n' "$plain" | grep -F "${pkg}:test:unit:" || true)
      printf '%s\n' "$pkg_lines" | grep -oE 'FAIL +[^ ]+\.test\.(ts|mjs)' | sort -u | sed "s|^|${pkg} |"
      # The stack terminus, from the same package's lines only.
      printf '%s\n' "$pkg_lines" | grep -oE 'at [A-Za-z][A-Za-z0-9_]* \(/[^)]*\)' | head -2 | sed "s|^|${pkg} |"
    done
    printf '%s\n' "${RESULTS[@]}" | grep '^FAIL' | sed 's/^/leg: /'
  } | sed 's/^/  /' | head -14
fi
echo "======================="
exit $rc

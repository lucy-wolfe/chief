// #941: locks the ordering properties scripts/gate-matrix.sh exists to
// guarantee. Mirrors ci-sequence.test.mjs's shape — derive line positions
// from the REAL script text, never from a remembered order — because the
// safety this driver buys for a shared CARGO_TARGET_DIR is entirely about
// sequence: cache-state must be provably fresh before cargo test/typecheck
// run, and #914's `verify` must run right before `test:unit` consumes the
// binaries `record` stamped.

import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import assert from "node:assert/strict";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..", "..");
const scriptPath = join(repoRoot, "scripts", "gate-matrix.sh");

function readScript() {
  return readFileSync(scriptPath, "utf8");
}

/** First 1-based line number whose text contains `needle`, or -1. */
function lineOf(text, needle) {
  const lines = text.split("\n");
  const idx = lines.findIndex((l) => l.includes(needle));
  return idx === -1 ? -1 : idx + 1;
}

test("the real gate-matrix.sh exists and is non-trivial", () => {
  const text = readScript();
  assert.ok(text.length > 500, "gate-matrix.sh looks too short to be the real driver");
});

test("gate-matrix.sh does NOT self-supply CI — a driver that does makes its own preflight check unfalsifiable", () => {
  // INVERTED (was: asserted the export exists). The merger proved against
  // its own driver that a driver supplying CI=1 itself makes the
  // CI-preflight check unfalsifiable in the run it's meant to protect —
  // #934's guard sat dead through every batch because of exactly this
  // shape. CI must come from the CALLER only; this driver refuses instead.
  const codeLines = readScript()
    .split("\n")
    .filter((l) => !l.trim().startsWith("#"));
  const code = codeLines.join("\n");
  assert.doesNotMatch(code, /^\s*export CI=/m, "gate-matrix.sh must NOT self-supply CI — that makes the preflight's CI check unfalsifiable in the run it protects");
});

test("an unset CI causes the real gate-matrix.sh to refuse, naming the CI reason, before any build runs", () => {
  const root = mkdtempSync(join(tmpdir(), "gate-matrix-ci-refuse-"));
  try {
    mkdirSync(join(root, "scripts", "lib"), { recursive: true });
    writeFileSync(join(root, "scripts/cargo-test-workspace.sh"), "#!/bin/sh\nexit 0\n");
    // Copy the real gate-preflight.sh so this exercises the real refusal
    // path, not a stand-in.
    const realPreflight = readFileSync(join(repoRoot, "scripts", "gate-preflight.sh"), "utf8");
    writeFileSync(join(root, "scripts/gate-preflight.sh"), realPreflight);
    let out = "";
    let status = 0;
    try {
      out = execFileSync("bash", [scriptPath, root], {
        encoding: "utf8",
        env: {
          ...process.env,
          CI: "",
          GATE_PREFLIGHT_MIN_FREE_GB: "1",
          // Keep the driver's own scratch directory inside the fixture. The
          // shared default is /root/cargo-targets-shared, which is not
          // writable for an unprivileged runner and produced a stray
          // "mkdir: cannot create directory '/root'" in the captured output.
          CARGO_TARGET_DIR: join(root, "cargo-target"),
        },
      });
    } catch (error) {
      status = error.status ?? -1;
      out = `${error.stdout ?? ""}${error.stderr ?? ""}`;
    }
    assert.equal(status, 1, out);
    assert.match(out, /REFUSING TO GATE: CI is unset/, "the refusal must name ITS CAUSE, not just exit non-zero — exit code alone was the merger's own near-miss");
    assert.doesNotMatch(out, /cargo-cache-state\.mjs/, "no build step may have run before the CI refusal");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// #1041: the CI test-binary SET, derived from ci.yml rather than restated.
//
// This driver exists to be at least as strict as CI. It was not: ci.yml's
// build-chiefd job builds `--bin chief --bin chiefd --bin beacond`
// and its test-unit job chmod +x's all three, while gate-matrix.sh built and
// provisioned only `chiefd` and `beacond`, and gate-preflight.sh's post arm
// checked only those two — under a message that claimed to match ci.yml.
// `chiefd` is the operator client; `chiefd` is the backend
// `resolveChiefdDaemonBinary` boots, so the local gate ran `test:unit`
// against a tree where 13 suites die on "chiefd binary not found at
// .../debug/chiefd". A seat reading that would see thirteen red
// suites and go looking for a regression that does not exist.
//
// Four places have to agree on the binary NAMES, and none of them is this
// file's opinion: CI's debug-binary chmod line is the authority, and the three local sites are checked against
// it. Add a fourth binary to ci.yml and this fails until the driver
// provisions it too.
function ciProvisionedBinaries() {
  const workflow = readFileSync(join(repoRoot, ".github", "workflows", "ci.yml"), "utf8");
  const line = workflow
    .split("\n")
    .find((l) => l.includes("chmod +x") && l.includes("apps/chiefd/target/debug/"));
  assert.ok(line, "ci.yml has no `chmod +x apps/chiefd/target/debug/...` line to derive the binary set from");
  const names = [...line.matchAll(/apps\/chiefd\/target\/debug\/([A-Za-z0-9_-]+)/g)].map((m) => m[1]);
  assert.ok(names.length > 0, "derived an EMPTY CI-binary set from ci.yml");
  return [...new Set(names)].sort();
}

test("#1041: gate-matrix.sh builds, provisions and preflight-checks EXACTLY the binary names ci.yml provisions", () => {
  const expected = ciProvisionedBinaries();
  const text = readScript();

  const buildLine = text.split("\n").find((l) => !l.trim().startsWith("#") && l.includes("--bin "));
  assert.ok(buildLine, "no `--bin` debug test build invocation found in gate-matrix.sh");
  const built = [...new Set([...buildLine.matchAll(/--bin ([A-Za-z0-9_-]+)/g)].map((m) => m[1]))].sort();
  assert.deepEqual(built, expected, "gate-matrix.sh's debug test build must build exactly the binaries ci.yml provisions");

  const provisionLoop = text.match(/mkdir -p "\$ROOT\/apps\/chiefd\/target\/debug"\nfor bin in ([^;]+); do/);
  assert.ok(provisionLoop, "no in-repo binary provisioning loop found in gate-matrix.sh");
  assert.deepEqual(
    provisionLoop[1].trim().split(/\s+/).sort(),
    expected,
    "gate-matrix.sh must provision in-repo exactly the binaries ci.yml chmods"
  );

  const preflight = readFileSync(join(repoRoot, "scripts", "gate-preflight.sh"), "utf8");
  const checkLoop = preflight.match(/for bin in ([^;]+); do\n\s*p="\$ROOT\/apps\/chiefd\/target\/debug\/\$bin"/);
  assert.ok(checkLoop, "no debug test-binary presence loop found in gate-preflight.sh");
  assert.deepEqual(
    checkLoop[1].trim().split(/\s+/).sort(),
    expected,
    "gate-preflight.sh's post arm must require exactly the binaries ci.yml provisions — it claims to match ci.yml in its own refusal message"
  );
});

test("CARGO_TARGET_DIR default is a shared, persistent path — not packet-keyed", () => {
  const text = readScript();
  assert.match(
    text,
    /CARGO_TARGET_DIR:=\/root\/cargo-targets-shared/,
    "default target dir must be a fixed shared path, not one derived from a packet id/branch/PID"
  );
});

test("pre-build preflight arm runs BEFORE the debug test build; post-build arm runs AFTER binary provisioning", () => {
  const text = readScript();
  const prePreflight = lineOf(text, 'gate-preflight.sh" "$ROOT" pre');
  const build = lineOf(text, "cargo-cache-state.mjs\" build");
  // #1041: the needle is the loop's `cp`, not the old per-binary line. The
  // provisioning step became a loop when `chiefd` joined the set; the
  // binary NAMES live in the derived assertion above, so this one only has to
  // locate the copy.
  const copy = lineOf(text, 'cp "$CARGO_TARGET_DIR/debug/$bin"');
  const postPreflight = lineOf(text, 'gate-preflight.sh" "$ROOT" post');
  assert.notEqual(prePreflight, -1, "no pre-build preflight arm call found");
  assert.notEqual(copy, -1, "no in-repo binary provisioning step found");
  assert.notEqual(postPreflight, -1, "no post-build preflight arm call found");
  assert.ok(prePreflight < build, `pre-build preflight (line ${prePreflight}) must precede the build (line ${build})`);

  // #1041: and it must precede the driver's FIRST WRITE to the host, not
  // merely the build. gate-preflight.sh's host refusal ends with "Nothing
  // was built, installed, or compiled"; with the preflight below this line
  // the driver had already created /root/cargo-targets-shared on a machine
  // it was in the act of refusing to gate on, which makes that sentence
  // false. The host check is the one precondition that must cost nothing.
  const targetDirMkdir = lineOf(text, 'mkdir -p "$CARGO_TARGET_DIR"');
  assert.notEqual(targetDirMkdir, -1, "no CARGO_TARGET_DIR creation step found");
  assert.ok(
    prePreflight < targetDirMkdir,
    `pre-build preflight (line ${prePreflight}) must precede the first write to this host — mkdir CARGO_TARGET_DIR (line ${targetDirMkdir})`
  );
  assert.ok(build < copy, `build (line ${build}) must precede binary provisioning (line ${copy})`);
  assert.ok(
    copy < postPreflight,
    `binary provisioning (line ${copy}) must precede the post-build preflight arm (line ${postPreflight}) — its own binary check requires them already in place`
  );
});

test("the post-build preflight call is handed CARGO_CACHE_STATE_SINCE_MS set to this run's own start, not a bare call", () => {
  const text = readScript();
  const postPreflightCall = text
    .split("\n")
    .find((l) => l.includes('gate-preflight.sh" "$ROOT" post'));
  assert.ok(postPreflightCall, "no post-build preflight invocation found");
  assert.match(
    postPreflightCall,
    /CARGO_CACHE_STATE_SINCE_MS="\$GATE_START_MS"/,
    "preflight must be handed this run's own start timestamp, or a leftover stamp from an earlier gate on the same shared dir could satisfy it"
  );
});

test("preflight (post arm) runs after binary provisioning, before cargo test / typecheck / verify / test:unit", () => {
  // Needles are the actual INVOCATION lines, not any mention — the header
  // prose above each step names these scripts too, and a bare substring
  // match would anchor on the comment instead of the real call.
  const text = readScript();
  const postPreflight = lineOf(text, 'gate-preflight.sh" "$ROOT" post');
  const cargoTest = lineOf(text, 'bash "$ROOT/scripts/cargo-test-workspace.sh"');
  const typecheck = lineOf(text, 'bash "$ROOT/scripts/typecheck.sh"');
  const verify = lineOf(text, 'verify_artifacts "cargo-test-workspace.sh"');
  const testUnit = lineOf(text, "TURBO_FORCE=true bun run test");
  for (const [label, line] of [["cargo test", cargoTest], ["typecheck", typecheck], ["verify", verify], ["test:unit", testUnit]]) {
    assert.notEqual(line, -1, `no ${label} invocation found`);
    assert.ok(postPreflight < line, `post-build preflight (line ${postPreflight}) must precede ${label} (line ${line})`);
  }
});

test("lint and the derived guard corpus (scripts/gate-matrix-legs.mjs) are both present", () => {
  const text = readScript();
  assert.match(text, /run "lint" bun run lint/, "no lint leg found");
  assert.match(text, /gate-matrix-legs\.mjs" --root "\$ROOT"/, "no derived guard corpus invocation found");
});

test("#914 verify runs IMMEDIATELY BEFORE EACH CONSUMER, and refuses rather than recording a failure", () => {
  // STRENGTHENED, not relaxed. The previous version asserted a single verify
  // between the build and test:unit, and additionally asserted that verify
  // should sit AFTER "the legs that don't consume the CI test binaries",
  // naming cargo-test-workspace.sh among them. That premise is false and was
  // proven false end-to-end on 051d6896c: cargo-test-workspace.sh IS a
  // consumer -- with chiefd at 0 bytes it exits 101 with "Exec format error",
  // and with the binary restored it exits 0. The old shape also could not
  // catch the observed failure at all: record passed, verify passed, and
  // test:unit then destroyed the binary itself, which the NEXT consumer ate.
  // A single verify proves a property that EXPIRES.
  const text = readScript();
  const build = lineOf(text, 'cargo-cache-state.mjs" build');
  const record = lineOf(text, 'cargo-target-dir-agreement.mjs" record');
  assert.notEqual(record, -1, "no #914 record step found");
  assert.ok(build < record, `build (${build}) must precede record (${record})`);

  // Every consumer of the CI test binaries must be immediately preceded by a
  // verify_artifacts call naming it.
  const consumers = ["cargo-test-workspace.sh", "test:unit", "derived guard corpus"];
  for (const c of consumers) {
    const call = lineOf(text, `verify_artifacts "${c}"`);
    assert.notEqual(call, -1, `no verify_artifacts call before consumer "${c}"`);
    assert.ok(record < call, `record (${record}) must precede the verify before ${c} (${call})`);
  }
  const cargoTest = lineOf(text, 'bash "$ROOT/scripts/cargo-test-workspace.sh"');
  const testUnit = lineOf(text, "TURBO_FORCE=true bun run test");
  assert.ok(lineOf(text, 'verify_artifacts "cargo-test-workspace.sh"') < cargoTest);
  assert.ok(lineOf(text, 'verify_artifacts "test:unit"') < testUnit);

  // And it must REFUSE, not record a FAIL and continue: a gate that spots a
  // destroyed artifact and proceeds is reporting on a tree that no longer
  // exists. The exit must live inside verify_artifacts itself.
  const fn = text.slice(text.indexOf("verify_artifacts() {"), text.indexOf("\nrun() {"));
  assert.match(fn, /REFUSING TO CONTINUE/, "verify_artifacts must name its refusal");
  assert.match(fn, /exit 1/, "verify_artifacts must exit, not merely report");
});

test("test:unit runs with TURBO_FORCE=true — this driver's CARGO_TARGET_DIR is shared and persistent, so the turbo cache it meets was populated by EARLIER gates; forcing makes execution observed rather than assumed, and a cached green is not evidence a test ran", () => {
  const text = readScript();
  const testUnitCall = text
    .split("\n")
    .find((l) => !l.trim().startsWith("#") && l.includes("bun run test"));
  assert.ok(testUnitCall, "no bun run test invocation found");
  assert.match(
    testUnitCall,
    /TURBO_FORCE=true\s+bun run test\b/,
    "test:unit must run with TURBO_FORCE=true until #939 lands CI in turbo's cache hash — a persistent target dir is exactly the precondition for a poisoned cache to be inherited from a prior run"
  );
});

test("the fast release profile is NOT wired in — out of scope, rejected separately", () => {
  // Executable lines only — the header comment names these env vars to
  // explain why they're absent, which would false-positive a bare substring
  // match. Only an ASSIGNMENT (`FOO=`) on a non-comment line is the thing
  // that would actually turn the fast profile on.
  const codeLines = readScript()
    .split("\n")
    .filter((l) => !l.trim().startsWith("#"));
  const code = codeLines.join("\n");
  assert.doesNotMatch(code, /CARGO_PROFILE_RELEASE_LTO=/, "fast profile env var must not be set by this driver");
  assert.doesNotMatch(code, /CARGO_PROFILE_RELEASE_CODEGEN_UNITS=/, "fast profile env var must not be set by this driver");
});

test("no skip/disable flag exists for the two safety steps (fail-closed has no off switch)", () => {
  const text = readScript();
  assert.doesNotMatch(text, /SKIP_CACHE_STATE/i);
  assert.doesNotMatch(text, /SKIP_VERIFY/i);
  assert.doesNotMatch(text, /SKIP_RECORD/i);
});

test("cargo-test-workspace.sh is used, never a bare `cargo test --workspace`", () => {
  const codeLines = readScript()
    .split("\n")
    .filter((l) => !l.trim().startsWith("#"));
  const code = codeLines.join("\n");
  assert.match(code, /cargo-test-workspace\.sh/, "must invoke the repo's sanctioned no-fail-fast wrapper");
  // The literal invocation has a SPACE ("cargo test --workspace"); the
  // sanctioned wrapper's filename has a HYPHEN ("cargo-test-workspace.sh").
  // Only the space form is the thing this test forbids.
  assert.doesNotMatch(code, /cargo test --workspace/, "must not hand-roll a bare cargo test --workspace");
});

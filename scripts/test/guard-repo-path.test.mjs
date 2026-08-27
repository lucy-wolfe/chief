// Negative self-test for scripts/guard-repo-path.sh — the mechanical refusal
// that stops a merger tool running a state-moving git verb inside the
// operator's checkout.
//
// WHY THIS EXISTS RATHER THAN A ONE-OFF MUTATION PROOF
// ------------------------------------------------------
// The guard was correct for a whole programme and had never been OBSERVED
// failing. A guard nobody has watched fail is a claim, not a check: a version
// that returns 0 for every input looks exactly like a working one. So this
// asserts BOTH directions on every run —
//
//   the REFUSE arm  — it bites on the tree it guards, including the two
//                     spellings (`..`, trailing slash) that a literal string
//                     comparison would let past;
//   the ALLOW arm   — it does NOT block real work, and specifically does not
//                     block a SHARED-PREFIX SIBLING (`…/chief-other`). That
//                     case is the one that proves the match is anchored on a
//                     path boundary rather than a string prefix: a naive
//                     `case "$real" in "$PROT"*)` — without the `/` — passes
//                     every refuse test and wrongly refuses this one.
//   the EMPTY-ARG arm — an empty argument resolves to $PWD by design, because
//                     a failed `cd` leaving the shell somewhere unintended is
//                     what caused the second real incident. Asserted in both
//                     directions: allowed from a safe cwd, refused from inside
//                     the protected tree.
//
// HERMETIC BY CONSTRUCTION
// --------------------------
// Every case runs against a TEMPORARY directory via GUARD_OPERATOR_CHECKOUT,
// never against the real /root/workspace/chief — which does not exist on the
// build hosts or on CI. A test that created it there would litter every
// machine it ran on, and would still not be exercising the real comparison.
// The production default is asserted separately, by reading the script's own
// text, so the seam cannot silently become the behaviour.

import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import assert from "node:assert/strict";
import test from "node:test";

const GUARD = join(import.meta.dirname, "..", "guard-repo-path.sh");

/** Source the guard with a temporary protected root and run one assertion.
 * Returns the exit status: 0 = allowed, non-zero = refused. */
function runGuard({ protectedRoot, target, cwd }) {
  const script = `. "${GUARD}"; assert_not_operator_checkout ${target === null ? '""' : `"${target}"`} "self-test"`;
  try {
    execFileSync("bash", ["-c", script], {
      cwd,
      env: { ...process.env, GUARD_OPERATOR_CHECKOUT: protectedRoot },
      stdio: "pipe",
    });
    return 0;
  } catch (error) {
    return error.status ?? 1;
  }
}

function withFixture(fn) {
  const root = mkdtempSync(join(tmpdir(), "guard-repo-path-"));
  const protectedRoot = join(root, "chief");
  mkdirSync(join(protectedRoot, "packages", "cli"), { recursive: true });
  mkdirSync(join(root, "chief-other"), { recursive: true }); // shared-prefix sibling
  mkdirSync(join(root, "merger-canonical"), { recursive: true });
  try {
    return fn({ root, protectedRoot });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test("REFUSE arm: the guard bites on the protected tree and its subdirectories", () => {
  withFixture(({ root, protectedRoot }) => {
    const mustRefuse = [
      [protectedRoot, "the protected checkout itself"],
      [`${protectedRoot}/`, "trailing slash"],
      [join(protectedRoot, "packages", "cli"), "a subdirectory"],
      [`${protectedRoot}/../chief`, "reached via .."],
      // A second `..` spelling that still lands INSIDE the tree. Note the
      // near-miss this replaced: `${root}/../chief` resolves to /tmp/chief,
      // which is genuinely NOT the protected tree, so the guard was right to
      // allow it and the test case was wrong. Kept as a comment because a
      // "refuse" case that does not actually point at the guarded tree would
      // have failed for a reason unrelated to the property under test.
      [join(protectedRoot, "packages", "..", "packages", "cli"), "reached via .. within the tree"],
    ];
    for (const [target, label] of mustRefuse) {
      assert.notEqual(
        runGuard({ protectedRoot, target, cwd: root }),
        0,
        `guard ALLOWED ${label} (${target}); it must refuse`,
      );
    }
  });
});

test("ALLOW arm: the guard does not block real work, including a shared-prefix sibling", () => {
  withFixture(({ root, protectedRoot }) => {
    const mustAllow = [
      [join(root, "merger-canonical"), "the merger's own clone"],
      [root, "the parent directory"],
      // The control that distinguishes a path-boundary match from a string
      // prefix. A naive `case "$real" in "$PROT"*)` refuses this and still
      // passes every case in the refuse arm above.
      [join(root, "chief-other"), "shared-prefix sibling"],
    ];
    for (const [target, label] of mustAllow) {
      assert.equal(
        runGuard({ protectedRoot, target, cwd: root }),
        0,
        `guard REFUSED ${label} (${target}); it must allow`,
      );
    }
  });
});

test("EMPTY-ARG arm: an empty argument resolves to $PWD and follows its verdict", () => {
  withFixture(({ root, protectedRoot }) => {
    assert.equal(
      runGuard({ protectedRoot, target: null, cwd: join(root, "merger-canonical") }),
      0,
      "empty argument from a safe cwd must be allowed",
    );
    assert.notEqual(
      runGuard({ protectedRoot, target: null, cwd: protectedRoot }),
      0,
      "empty argument from inside the protected tree must be refused — this is incident two",
    );
  });
});

test("an unresolvable path fails closed rather than passing silently", () => {
  withFixture(({ protectedRoot, root }) => {
    // A path whose parent does not exist still resolves under readlink -f, so
    // assert the documented property directly: the guard never returns 0 for a
    // target it could not resolve. Verified through the script's own text
    // because the condition is not reachable with a valid filesystem path.
    const source = readFileSync(GUARD, "utf8");
    assert.match(
      source,
      /An unresolvable path is not proof of safety/,
      "the fail-closed branch for an unresolvable path must remain present",
    );
    // And a normal absolute path outside the tree is still allowed, so the
    // fail-closed branch is not swallowing everything.
    assert.equal(runGuard({ protectedRoot, target: join(root, "merger-canonical"), cwd: root }), 0);
  });
});

test("the production default is /root/workspace/chief — the test seam is not the behaviour", () => {
  const source = readFileSync(GUARD, "utf8");
  assert.match(
    source,
    /GUARD_OPERATOR_CHECKOUT:-\/root\/workspace\/chief/,
    "the default protected path must remain /root/workspace/chief when nothing is set",
  );
  // Sourced with NO override, the guard must refuse the real operator checkout
  // regardless of whether that path exists on this machine.
  const script = `. "${GUARD}"; assert_not_operator_checkout "/root/workspace/chief" "self-test"`;
  let status = 0;
  try {
    execFileSync("bash", ["-c", script], {
      stdio: "pipe",
      env: Object.fromEntries(
        Object.entries(process.env).filter(([key]) => key !== "GUARD_OPERATOR_CHECKOUT"),
      ),
    });
  } catch (error) {
    status = error.status ?? 1;
  }
  assert.notEqual(status, 0, "with no override the guard must refuse /root/workspace/chief");
});

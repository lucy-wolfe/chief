// The cold proof `scripts/cold-start-latency.mjs` refuses to print a number
// without, driven against WARM fixtures so that it must refuse.
//
// # The defect this closes
//
// `proveCold` asserted the absence of three paths — `~/.chiefd/orgs/
// .<slug>.chief.db`, `~/.chiefd/orgs/<slug>` and `~/.chiefd/run/<slug>.log` —
// and one tmux session named `org-<slug>_`. A company is a DIRECTORY now, its
// state is `<dir>/.chief/`, and its session carries the company key, so every
// one of those four assertions was over a set that CANNOT be non-empty. The
// gate passed unconditionally while printing "COLD" for all five rows.
//
// That is the worst failure a benchmark's guard can have: the harness exists
// because this exact measurement was already ruined once by a warm stack
// (1.77s on unfixed code, because only the actuator had been killed), and the
// proof that was supposed to prevent a repeat had quietly stopped asking.
//
// So every rule below is checked TWICE — once against a cold fixture, where it
// must find nothing, and once against a warm one, where it must find the thing.
// A rule with only the cold half would pass with its body deleted.
//
// Run with `node --test scripts/test/cold-start-latency.test.mjs`.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  argvNamesDaemonFor,
  coldDirectoryAssertions,
  coldSessionAssertions,
  companyChiefDir,
  daemonLogPath,
  installHome,
  rendezvousPath,
  sessionNamesFor,
  violations,
} from "../cold-start-latency-lib.mjs";

const SLUG = "coldstart-1";
/** The key beacond serves for the fixture directory. Deliberately not a hash
 *  of it: the harness reads the key off the daemon's own rendezvous. */
const KEY = "abc123def456";

const scratch = [];
function fixtureDir() {
  const dir = mkdtempSync(join(tmpdir(), "cold-start-guard-"));
  scratch.push(dir);
  return dir;
}
function cleanup() {
  for (const dir of scratch.splice(0)) rmSync(dir, { recursive: true, force: true });
}

test("the box's chief directory is `~/.chief`, and a company's state is its own", () => {
  assert.equal(installHome("/home/op"), "/home/op/.chief");
  assert.equal(companyChiefDir("/work/anvils"), "/work/anvils/.chief");
  assert.equal(daemonLogPath("/work/anvils"), "/work/anvils/.chief/run/daemon.log");
  assert.equal(rendezvousPath("/work/anvils"), "/work/anvils/.chief/run/daemon.json");
});

test("a session name carries the slug, six key characters and the terminator", () => {
  const { company, actuator } = sessionNamesFor({ slug: SLUG, key: KEY });
  assert.equal(company, `org-${SLUG}-abc123_`);
  assert.equal(actuator, `chiefd-actuator-org-${SLUG}-abc123_`);
});

test("a session name cannot be built without the SERVED key", () => {
  // The property the whole split cold proof rests on: there is no name to
  // look for until the company exists, so the harness must not pretend there
  // is. A silently-empty key would produce `org-<slug>-_`, which matches
  // nothing and would make the session half of the proof vacuous again.
  assert.throws(() => sessionNamesFor({ slug: SLUG, key: "" }), /company key/);
  assert.throws(() => sessionNamesFor({ slug: SLUG, key: "abc" }), /company key/);
  assert.throws(() => sessionNamesFor({ slug: "", key: KEY }), /slug/);
});

test("a daemon argv is matched on ADJACENT WHOLE TOKENS, so a sibling directory is not this one", (t) => {
  t.after(cleanup);
  const dir = "/srv/companies/acme";
  assert.equal(argvNamesDaemonFor(`/root/.chief/bin/chiefd run --dir ${dir}`, dir), true);
  assert.equal(
    argvNamesDaemonFor(`/root/.chief/bin/chiefd run --dir ${dir} --launcher-root /repo`, dir),
    true,
  );
  // The collision this exists for: `…/acme` is a prefix of `…/acme-corp`.
  assert.equal(argvNamesDaemonFor(`/root/.chief/bin/chiefd run --dir ${dir}-corp`, dir), false);
  // And adjacency, so a stray occurrence of the path elsewhere in an argv is
  // not read as this company's daemon.
  assert.equal(argvNamesDaemonFor(`chief actuate --launcher-root ${dir} run`, dir), false);
});

test("the directory half of the cold proof: cold finds nothing, WARM refuses", (t) => {
  t.after(cleanup);
  const cold = fixtureDir();
  assert.deepEqual(
    violations(coldDirectoryAssertions({ dir: cold, exists: existsSync, processTable: [] })),
    [],
    "a freshly minted directory with no processes is cold",
  );

  // WARM 1: the directory already holds a company. `<dir>/.chief` is the
  // complete footprint, so this is the whole "has anything ever run here"
  // question in one assertion.
  const warm = fixtureDir();
  mkdirSync(join(companyChiefDir(warm), "db"), { recursive: true });
  writeFileSync(join(companyChiefDir(warm), "db", "chief.db"), "fixture");
  const held = violations(coldDirectoryAssertions({ dir: warm, exists: existsSync, processTable: [] }));
  assert.equal(held.length, 1, `a directory holding a company is NOT cold: ${JSON.stringify(held)}`);
  assert.equal(held[0].claim, "the directory holds no company");

  // WARM 2: a daemon is already serving this directory.
  const served = violations(
    coldDirectoryAssertions({
      dir: cold,
      exists: existsSync,
      processTable: [
        { pid: 4242, args: `/root/.chief/bin/chiefd run --dir ${cold} --launcher-root /repo` },
        // A SIBLING's daemon, which must not count as this company's.
        { pid: 4243, args: `/root/.chief/bin/chiefd run --dir ${cold}-corp` },
      ],
    }),
  );
  assert.equal(served.length, 1, "a daemon already serving this directory is NOT cold");
  assert.equal(served[0].claim, "no daemon process for this directory");
  assert.deepEqual(
    served[0].observed,
    [`4242 /root/.chief/bin/chiefd run --dir ${cold} --launcher-root /repo`],
    "only THIS directory's daemon is a violation — the sibling is a different company",
  );
});

test("the session half of the cold proof: cold finds nothing, WARM refuses", () => {
  const { company, actuator } = sessionNamesFor({ slug: SLUG, key: KEY });

  assert.deepEqual(
    violations(coldSessionAssertions({ sessions: ["0", "org-other-ffffff_"], slug: SLUG, key: KEY })),
    [],
    "another company's session is not this company's",
  );

  const painted = violations(coldSessionAssertions({ sessions: ["0", company], slug: SLUG, key: KEY }));
  assert.equal(painted.length, 1, "a live company session means somebody is already actuating it");
  assert.equal(painted[0].claim, "no company tmux session");

  const actuating = violations(coldSessionAssertions({ sessions: [actuator], slug: SLUG, key: KEY }));
  assert.equal(actuating.length, 1, "a live actuator session is a warm stack");
  assert.equal(actuating[0].claim, "no actuator tmux session");

  // THE REGRESSION ITSELF. The retired name — no key, no discriminator — must
  // not satisfy the assertion, because a proof that looks for a name nothing
  // is ever called finds nothing on the warmest box in the fleet.
  assert.deepEqual(
    violations(coldSessionAssertions({ sessions: [`org-${SLUG}_`], slug: SLUG, key: KEY })),
    [],
    "sanity: the retired spelling is simply a different session, not this company's",
  );
  assert.notEqual(company, `org-${SLUG}_`, "the live name must not be the retired one");
});

test("the harness reads its rules from this one library", async () => {
  // The instrument itself is not importable — it runs its `main()` at module
  // scope, by design, because it is an operator entrypoint. What is checkable
  // is that it does not carry a SECOND copy of any rule tested above.
  const source = await import("node:fs").then((fs) =>
    fs.readFileSync(new URL("../cold-start-latency.mjs", import.meta.url), "utf8"),
  );
  assert.match(source, /from "\.\/cold-start-latency-lib\.mjs"/, "it must import the rules");
  for (const retired of [".chiefd", "--company", "orgs"]) {
    assert.ok(
      !source.includes(retired),
      `cold-start-latency.mjs still names the retired \`${retired}\` — every one of those is a path or ` +
        "an argument no company has, and an assertion over one of them is an assertion over nothing",
    );
  }
  assert.ok(
    !/org-\$\{slug\}_/.test(source),
    "the retired session spelling is back, and it matches no live session",
  );
});

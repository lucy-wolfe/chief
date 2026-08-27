import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/**
 * #112: the promote script's REFUSALS are the product.
 *
 * The happy path was already proven in production. What nearly shipped an empty
 * deploy was a FAILURE THAT DID NOT STOP THE SEQUENCE: `cp` hit ETXTBSY, printed
 * one line, and the restart ran anyway on the old binary — passing every boot
 * check, because those verify that the daemon booted correctly, not that it is
 * new. Each case below is a real incident shape and asserts a NON-ZERO exit with
 * a message naming the reason: a guard that aborts silently is as bad as none.
 */
const SCRIPT = join(import.meta.dir, "..", "scripts", "promote-chiefd.sh");
const CONTROL = "supervision cycle committed";

function sandbox() {
  const dir = mkdtempSync(join(tmpdir(), "promote-"));
  const start = join(dir, "start.sh");
  writeFileSync(start, "#!/usr/bin/env bash\nexit 0\n");
  chmodSync(start, 0o755);
  fakeBinary(join(dir, "beacond"), ["beacond fixture"]);
  return { dir, start };
}

/** A stand-in "binary": `strings` finds any literal written into it. */
function fakeBinary(path: string, literals: string[]) {
  writeFileSync(path, `${literals.join("\n")}\n`);
  chmodSync(path, 0o755);
}

async function run(args: string[], environment: Record<string, string | undefined> = {}) {
  const proc = Bun.spawn(["bash", SCRIPT, ...args], {
    stdout: "pipe",
    stderr: "pipe",
    env: { ...process.env, ...environment },
  });
  const [out, err] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
  ]);
  return { code: await proc.exited, text: out + err };
}

describe("#112 — the promote script refuses rather than shipping nothing", () => {
  test("beacond replacement escalates and proves the old daemon is gone", () => {
    const script = readFileSync(SCRIPT, "utf8");
    expect(script).toContain('kill -TERM "$BEACOND_PID"');
    expect(script).toContain('kill -KILL "$BEACOND_PID"');
    expect(script).toContain('old beacond pid $BEACOND_PID survived SIGTERM and SIGKILL');
    expect(script.indexOf('old beacond pid $BEACOND_PID survived')).toBeLessThan(
      script.indexOf('setsid nohup env BEACOND_DB_PATH='),
    );
  });

  test("promotion names beacond as the selected-company authority and keeps argv fallback scoped", () => {
    const script = readFileSync(SCRIPT, "utf8");
    expect(script).toContain("Beacond's company registration is the authority for the selected daemon.");
    expect(script).toContain('company_field pid');
    expect(script).toContain('pgrep -f "$(basename "$TARGET") run --dir $COMPANY_DIR"');
    expect(script).toContain('lookup?dir=$COMPANY_DIR');
    expect(script).not.toContain("The listener pid comes from `ss`, never `pgrep`.");
  });

  test("single-instance verification uses the company-scoped live-process counter", () => {
    const script = readFileSync(SCRIPT, "utf8");
    expect(script).toContain('source "$HERE/lib/company-process-count.sh"');
    expect(script).toContain('company_chiefd_instance_count "$TARGET" "$COMPANY_DIR"');
    expect(script).not.toContain("ps -eo comm");
  });

  test("a marker ABSENT from the new binary aborts: the build did not include the change", async () => {
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL]);
    fakeBinary(target, [CONTROL]);
    const { code, text } = await run([
      "--built", built, "--target", target, "--start", start, "--marker", "brand new line",
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("ABSENT from the newly built binary");
  });

  test("a marker ALREADY in the outgoing binary aborts: it is not a differential", async () => {
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL, "shared marker"]);
    fakeBinary(target, [CONTROL, "shared marker"]);
    const { code, text } = await run([
      "--built", built, "--target", target, "--start", start, "--marker", "shared marker",
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("ALREADY in the outgoing binary");
  });

  test("a missing POSITIVE CONTROL aborts: an unvalidated probe's other answers mean nothing", async () => {
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, ["new marker"]);
    fakeBinary(target, []);
    const { code, text } = await run([
      "--built", built, "--target", target, "--start", start, "--marker", "new marker",
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("positive control");
  });

  test("a missing built binary aborts before anything is touched", async () => {
    const { dir, start } = sandbox();
    const { code, text } = await run([
      "--built", join(dir, "nope"), "--target", join(dir, "live"), "--start", start,
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("built binary is missing");
  });

  test("incomplete arguments abort rather than defaulting to something plausible", async () => {
    const { start } = sandbox();
    const { code, text } = await run(["--start", start]);
    expect(code).not.toBe(0);
    expect(text).toContain("are all required");
  });

  // The positive half: with a valid differential the checks PASS and the script
  // proceeds past them. Without this, every assertion above would still hold if
  // the script simply aborted unconditionally.
  test("a genuine differential passes the artifact checks and proceeds to promote", async () => {
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL, "a literal this change introduced"]);
    fakeBinary(target, [CONTROL]);
    const { text } = await run([
      "--built", built, "--target", target, "--start", start,
      "--marker", "a literal this change introduced", "--check-only",
    ]);
    expect(text).toContain("positive control present");
    expect(text).toContain("ARTIFACT CHECKS PASSED");
    expect(text).not.toContain("DEPLOY ABORTED");
  });

  test("a real manual restart REFUSES before promotion when the company's own store is absent", async () => {
    const { dir, start } = sandbox();
    const home = mkdtempSync(join(tmpdir(), "promote-no-chiefd-home-"));
    const companyDir = join(home, "companies", "northstar");
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL, "manual-store-probe"]);
    fakeBinary(target, [CONTROL]);
    try {
      const { code, text } = await run([
        "--built", built, "--target", target, "--start", start, "--marker", "manual-store-probe", "--dir", companyDir,
      ], { HOME: home });
      expect(code).not.toBe(0);
      expect(text).toContain(`canonical company database is missing: ${join(companyDir, ".chief", "db", "chief.db")}`);
      // The store gate is before rename(2): no-docstore manual restart must not
      // replace the known-good live binary on its way to a broken daemon.
      expect(readFileSync(target, "utf8")).toContain(CONTROL);
      expect(readFileSync(target, "utf8")).not.toContain("manual-store-probe");
    } finally {
      rmSync(home, { recursive: true, force: true });
    }
  });

  test("without --check-only, a missing --dir is refused before anything is promoted", async () => {
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL, "no-company-probe"]);
    fakeBinary(target, [CONTROL]);
    const { code, text } = await run([
      "--built", built, "--target", target, "--start", start, "--marker", "no-company-probe",
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("--dir <company directory> is required unless --check-only is used");
    // Refused before rename(2): the live binary must be untouched.
    expect(readFileSync(target, "utf8")).toContain(CONTROL);
    expect(readFileSync(target, "utf8")).not.toContain("no-company-probe");
  });

  test("`--company` is gone, and an unknown flag aborts rather than being ignored", async () => {
    // The whole point of deleting it: a leftover `--company acme` in an
    // operator's shell history must not promote against a default company.
    const { dir, start } = sandbox();
    const built = join(dir, "built");
    const target = join(dir, "live");
    fakeBinary(built, [CONTROL]);
    fakeBinary(target, [CONTROL]);
    const { code, text } = await run([
      "--built", built, "--target", target, "--start", start, "--company", "northstar",
    ]);
    expect(code).not.toBe(0);
    expect(text).toContain("unknown argument '--company'");
  });
});

// Test 13 (#768): the single-instance check must count only the requested
// company's daemon, driven against the REAL company_chiefd_instance_count
// shell function with real fixture processes -- not just a source-string
// assertion (covered separately above) -- so a future rewrite that keeps the
// source string but breaks the pattern is still caught.
describe("company_chiefd_instance_count (#768) — per-directory, not global", () => {
  const spawned: Bun.Subprocess[] = [];
  const COMPANIES = "/srv/companies";

  afterEach(async () => {
    for (const proc of spawned.splice(0)) {
      try { proc.kill("SIGKILL"); } catch { /* already exited */ }
      await proc.exited;
    }
  });

  /** A long-lived fixture process whose argv literally matches what a real
   *  `chiefd run --dir <company directory>` invocation's pgrep pattern looks
   *  for. */
  function fixtureCompanyProcess(dir: string): Bun.Subprocess {
    const proc = Bun.spawn(
      ["bash", "-c", `exec -a "chiefd run --dir ${dir}" sleep 30`],
      { stdout: "ignore", stderr: "ignore" },
    );
    spawned.push(proc);
    return proc;
  }

  async function countFor(dir: string): Promise<string> {
    const result = Bun.spawnSync([
      "bash", "-c",
      `source scripts/lib/company-process-count.sh; company_chiefd_instance_count chiefd "$1"`,
      "bash", dir,
    ], { cwd: join(import.meta.dir, "..") });
    return result.stdout.toString().trim();
  }

  async function waitForArgv(...dirs: string[]): Promise<void> {
    for (let attempt = 0; attempt < 40; attempt++) {
      const seen = Bun.spawnSync(["pgrep", "-af", "chiefd run --dir"]).stdout.toString();
      if (dirs.every((dir) => seen.includes(`${dir}\n`))) return;
      await Bun.sleep(50);
    }
  }

  test("counts exactly 1 for the requested directory with a second company's daemon also live", async () => {
    fixtureCompanyProcess(`${COMPANIES}/company-a`);
    fixtureCompanyProcess(`${COMPANIES}/company-b`);
    await waitForArgv(`${COMPANIES}/company-a`, `${COMPANIES}/company-b`);
    expect(await countFor(`${COMPANIES}/company-a`)).toBe("1");
  }, 15_000);

  test("a SIBLING whose directory extends this one is not this company's daemon", async () => {
    // Under one companies root `acme` and `acme-corp` are the ordinary shape,
    // and `--dir /srv/companies/acme` is a PREFIX of `--dir …/acme-corp`. An
    // unanchored pattern counts 2 here and a single-company promote refuses
    // itself for a company that is perfectly healthy.
    fixtureCompanyProcess(`${COMPANIES}/acme-corp`);
    await waitForArgv(`${COMPANIES}/acme-corp`);
    expect(await countFor(`${COMPANIES}/acme`)).toBe("0");
    // The positive half, so a pattern that matched NOTHING would fail here too.
    fixtureCompanyProcess(`${COMPANIES}/acme`);
    await waitForArgv(`${COMPANIES}/acme`);
    expect(await countFor(`${COMPANIES}/acme`)).toBe("1");
  }, 15_000);
});

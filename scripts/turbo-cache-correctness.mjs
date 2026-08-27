// #944: every gate on this program runs `test:unit` with `TURBO_FORCE=true`
// -- deliberately, because forcing is what stops a cached green from being
// mistaken for an executed one. The cost: no gate ever consults the cache
// key. A `turbo.json` change that destroys caching (#939's original bug --
// 8 per-run-varying vars in `env`) or one that caches when it should not (a
// result-affecting var left in `passThroughEnv`, or -- #945 -- silently
// missing from BOTH buckets) passes every gate this program has, because a
// permanent miss and a perfect hit both print `0 cached, N total` under
// `--force`.
//
// THE DESIGN THAT MATTERS: this checks the REAL `turbo.json` against the
// REAL `test:unit` task, not a synthetic fixture with its own config. A
// synthetic workspace proves turbo's own semantics work, which nobody
// doubts and which was never the failure -- #945's own defect (`CI`
// missing from every bucket) would have sailed straight through a toy
// workspace that happened to declare `CI` correctly. `turbo run <task>
// --dry=json --filter=<pkg>` computes and prints the task's cache hash
// WITHOUT executing anything, against whatever `turbo.json` actually says
// today. That sidesteps the other trap entirely (turbo never caches a
// FAILING task, so an executed probe that throws produces indistinguishable
// miss/miss/miss) -- nothing executes here, so nothing can fail.
//
// KNOWN LIMITATION, disclosed rather than silently claimed covered: this
// guard proves the real `turbo.json` declares the right bucket for a var
// GIVEN a stable, deterministic hash function -- it does NOT exercise cache
// behavior at EXECUTION time, because nothing here executes. An earlier
// version of this file used a synthetic fixture that DID execute the real
// `turbo run` command, and found exactly such an execution-time defect:
// running the task wrote `pkg/.turbo/turbo-run.log`, which the NEXT
// invocation's own default input glob then hashed as a file input, so two
// otherwise-identical executions produced two different hashes chasing
// their own tail, never converging to a hit. `--dry=json` never executes
// the task, so it never wrote that log and looked perfectly stable --
// invisible to this file by construction. Checked against the real
// workspace (dozens of real, non-dry `test:unit` runs across #939/#945/#944
// verification, forced and unforced): never reproduced there, likely
// because the real task declares no `outputs`/`inputs` override at all
// (the synthetic fixture had an explicit empty `outputs: []`), but this is
// an observation, not a proof it cannot happen on the real config -- filed
// separately for someone to isolate properly rather than lost with the
// harness that found it.
//
// For every var this repo's `turbo.json` declares for a task, in EITHER
// bucket, this toggles its value and re-derives the hash:
//   - a var in `env`            -> hash MUST differ when the value changes
//   - a var in `passThroughEnv` -> hash MUST stay identical when it changes
// Both directions carry a same-value CONTROL run first, so "differs" can
// never be confused with "hashing is broken generally" and "identical"
// can never be confused with "the dry-run just isn't picking up my
// override at all".

import { execFileSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import ts from "typescript";

const HERE = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(HERE, "..");

export function readTurboJson(turboJsonPath = join(REPO_ROOT, "turbo.json")) {
  const { config, error } = ts.readConfigFile(resolve(turboJsonPath), ts.sys.readFile);
  if (error) {
    throw new Error(`cannot read ${turboJsonPath}: ${ts.flattenDiagnosticMessageText(error.messageText, " ")}`);
  }
  return config ?? {};
}

// The effective task definition governing a package's execution of `task`.
// A package-specific `<pkg>#<task>` key REPLACES the generic entry rather
// than merging with it -- turbo's own resolution rule (the same one #939's
// audit had to account for), not something this file decides on its own.
export function effectiveTaskDef(turboConfig, packageName, task) {
  return turboConfig.tasks?.[`${packageName}#${task}`] ?? turboConfig.tasks?.[task] ?? {};
}

// `turbo run <task> --filter=<pkg> --dry=json` prints a banner line
// ("• turbo 2.10.0", sometimes a version-mismatch WARNING) before the JSON
// body -- the JSON itself starts at the first `{`. Deliberately not parsing
// with a regex for the hash: the exact JSON shape (`tasks[0].hash`) is part
// of turbo's own dry-run contract and worth reading structurally.
export function dryRunHash({ turboBin, task, pkg, env = {}, cwd = REPO_ROOT }) {
  const stdout = execFileSync(turboBin, ["run", task, `--filter=${pkg}`, "--dry=json"], {
    cwd,
    encoding: "utf8",
    env: { ...process.env, ...env, TURBO_UI: "0" }
  });
  const jsonStart = stdout.indexOf("{");
  if (jsonStart === -1) {
    throw new Error(`turbo --dry=json produced no JSON body at all:\n${stdout}`);
  }
  const parsed = JSON.parse(stdout.slice(jsonStart));
  const taskEntry = parsed.tasks?.find((t) => t.taskId === `${pkg}#${task}`) ?? parsed.tasks?.[0];
  if (!taskEntry?.hash) {
    throw new Error(`turbo --dry=json JSON had no task hash for ${pkg}#${task}:\n${JSON.stringify(parsed, null, 2)}`);
  }
  return taskEntry.hash;
}

// THE per-variable check. `bucket` is `"env"` (hash must differ) or
// `"passThroughEnv"` (hash must stay identical). Three dry-runs, not two:
// the control run (same value as run 1) proves the harness itself is
// producing a stable, comparable hash before the "changed" run's result is
// trusted either way.
function probeValue(varName, suffix) {
  // Absolute-path-shaped, not opaque strings: `HOME` broke turbo/bun's own
  // config resolution ("Path is not absolute") when given a bare non-path
  // string. `--dry=json` never reads a path's contents (nothing executes),
  // so an absolute path that doesn't exist is safe for every variable,
  // path-shaped or not -- EXCEPT `PATH` itself, which the spawned turbo
  // process needs intact to find `node`/`bun` to run at all. Prefixing the
  // probe onto the real `PATH` keeps every real entry resolvable while
  // still changing the value enough to move a hash that's supposed to move.
  if (varName === "PATH") return `/tmp/944-control-${suffix}:${process.env.PATH ?? ""}`;
  return `/tmp/944-control-${suffix}`;
}

export function checkVariable({ turboBin, task, pkg, varName, bucket, cwd = REPO_ROOT }) {
  const baseline = dryRunHash({ turboBin, task, pkg, cwd, env: { [varName]: probeValue(varName, "a") } });
  const control = dryRunHash({ turboBin, task, pkg, cwd, env: { [varName]: probeValue(varName, "a") } });
  const changed = dryRunHash({ turboBin, task, pkg, cwd, env: { [varName]: probeValue(varName, "b") } });

  const problems = [];
  if (control !== baseline) {
    problems.push(`${varName}: two dry-runs with the IDENTICAL value produced different hashes (${baseline} vs ${control}) -- the harness itself is not stable, not evidence about ${varName}`);
    return { conclusive: false, problems };
  }

  if (bucket === "env") {
    if (changed === baseline) {
      problems.push(`${varName} is declared in 'env' but changing its value did NOT change the hash (${baseline} both times) -- it is visible but unhashed, the exact #945 shape`);
    }
  } else if (bucket === "passThroughEnv") {
    if (changed !== baseline) {
      problems.push(`${varName} is declared in 'passThroughEnv' but changing its value changed the hash (${baseline} -> ${changed}) -- it is being hashed despite being declared as ambient-only, which will defeat caching on every invocation`);
    }
  } else {
    throw new Error(`checkVariable: unknown bucket '${bucket}' for ${varName}`);
  }

  return { conclusive: true, pass: problems.length === 0, problems };
}

// Runs `checkVariable` for every var declared in either bucket of the
// given task's effective definition. Returns one result per variable so a
// caller can report exactly which var(s) are wrong, not just that
// something is.
export function checkTaskEnvHashing({ turboBin, task, pkg, turboConfig = readTurboJson(), cwd = REPO_ROOT }) {
  const taskDef = effectiveTaskDef(turboConfig, pkg, task);
  const envVars = taskDef.env ?? [];
  const passThroughVars = taskDef.passThroughEnv ?? [];
  if (envVars.length === 0 && passThroughVars.length === 0) {
    throw new Error(`${pkg}#${task} (or the generic '${task}') declares no 'env'/'passThroughEnv' vars at all -- nothing to check (#944 vacuity)`);
  }
  const results = [];
  for (const varName of envVars) {
    results.push({ varName, bucket: "env", ...checkVariable({ turboBin, task, pkg, varName, bucket: "env", cwd }) });
  }
  for (const varName of passThroughVars) {
    results.push({ varName, bucket: "passThroughEnv", ...checkVariable({ turboBin, task, pkg, varName, bucket: "passThroughEnv", cwd }) });
  }
  return results;
}

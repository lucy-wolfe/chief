// The DECISIONS `scripts/cold-start-latency.mjs` makes, separated from the IO
// it makes them about.
//
// WHY THIS FILE EXISTS
// --------------------
// The harness refuses to print a number unless it can PROVE the start was
// cold, and that proof is the only thing standing between an operator and a
// confident wrong latency. A proof nobody can test is a claim, so every rule
// it applies lives here as a pure function over inputs the caller measured,
// and `scripts/test/cold-start-latency.test.mjs` drives each one against a
// WARM fixture and requires it to refuse.
//
// That test is not decoration. The previous version of the cold proof named
// `~/.chiefd/orgs/.<slug>.chief.db`, `~/.chiefd/orgs/<slug>` and
// `~/.chiefd/run/<slug>.log` — three paths that no longer exist under any
// company — so every assertion was over an empty set and `proveCold` PASSED
// UNCONDITIONALLY. A vacuous gate on a benchmark is how a regression gets
// certified, and it is invisible: the report still printed "COLD" in capital
// letters for all five rows.

import { dirname, join } from "node:path";

/**
 * The box's own chief directory: the `bin/` symlinks, the versioned installs
 * under `versions/`, beacond's registry and the box-wide jsonl sinks.
 *
 * `~/.chief`, not `~/.chiefd`. The latter was the global company tree behind a
 * slug registry; a company is a DIRECTORY now, so the tree is gone and only
 * install-level facts are left in `~`.
 * (`chief-cli::paths::install_home`, `release-chiefd.ts`, `beacond::config`.)
 */
export function installHome(home) {
  return join(home, ".chief");
}

/** The installed operator client. */
export function chiefBinary(home) {
  return join(installHome(home), "bin", "chief");
}

/**
 * The resources installed beside the binary at `path`.
 *
 * TOMBSTONE: `launcherRootRecord`, the `~/.chief/launcher-root` pointer. It
 * named a source CHECKOUT, which is what made an install a front end for a git
 * working copy. Resources live inside the version directory now, so the answer
 * is derived from the binary rather than recorded anywhere — the same
 * `<binary>/../../resources` expression `host_primitives::install` uses, and the
 * reason this harness can no longer be pointed at the wrong tree.
 */
export function resourceRootBeside(binaryPath) {
  return join(dirname(dirname(binaryPath)), "resources");
}

/**
 * EVERYTHING chief owns inside a company, `<dir>/.chief`.
 *
 * The complete footprint, which is what makes it the right cold assertion:
 * the store, the identity keys, the jsonl sinks and the disposable run
 * directory all hang off it (`chiefd_daemon::company_dir`), and `chief rm`
 * deletes exactly this. A directory without it has no company in any sense a
 * caller can act on.
 */
export function companyChiefDir(dir) {
  return join(dir, ".chief");
}

/** `chief-cli::paths::store_db_path` — the first-run check's own subject. */
export function companyStorePath(dir) {
  return join(companyChiefDir(dir), "db", "chief.db");
}

/** `chief-cli::paths::daemon_log_path` — a spawned daemon's stdout/stderr. */
export function daemonLogPath(dir) {
  return join(companyChiefDir(dir), "run", "daemon.log");
}

/** `host_primitives::rendezvous::rendezvous_path`. */
export function rendezvousPath(dir) {
  return join(companyChiefDir(dir), "run", "daemon.json");
}

/** How much of the company key a tmux session name carries.
 *  `chief-cli::placement::SESSION_KEY_CHARS`. */
export const SESSION_KEY_CHARS = 6;

/**
 * A company's tmux session name, and its actuator's.
 *
 * `org-<slug>-<first six of the company key>_`
 * (`chief-cli::placement::session_name_for`), and
 * `chiefd-actuator-` + that (`chief-cli::attach::actuator_session_name`).
 *
 * BOTH HALVES ARE LOAD-BEARING. The trailing `_` is the terminator that makes
 * a prefix collision between two companies structurally impossible — `tmux -t`
 * matches exactly first and falls back to PREFIX. The six key characters are
 * what make the name name ONE company: a tmux server is box-wide while a
 * company is a directory, so two directories holding companies called the same
 * thing would otherwise share a session.
 *
 * The key is SERVED — read off the daemon's own rendezvous — never hashed
 * here. A key that is merely CLOSE names a session no company has, and a
 * cold proof that asserts the absence of a name nothing can ever be called
 * passes for every company on the box, warm ones included.
 */
export function sessionNamesFor({ slug, key }) {
  if (typeof slug !== "string" || slug.length === 0) {
    throw new Error("a session name needs the company's slug");
  }
  if (typeof key !== "string" || key.length < SESSION_KEY_CHARS) {
    throw new Error(
      `a session name needs the company key beacond served, at least ${SESSION_KEY_CHARS} characters: got ${JSON.stringify(key)}`,
    );
  }
  const company = `org-${slug}-${key.slice(0, SESSION_KEY_CHARS)}_`;
  return { company, actuator: `chiefd-actuator-${company}` };
}

/**
 * Does this argv name a `chiefd run` for exactly `dir`?
 *
 * ADJACENT WHOLE TOKENS, never a substring. `--dir /srv/companies/acme` is a
 * PREFIX of `--dir /srv/companies/acme-corp`, and under one companies root
 * those two are the ordinary shape rather than a contrived one — a substring
 * test would report a neighbouring company's daemon as this one's. This only
 * FILTERS, so the cost is a corrupted benchmark number rather than a killed
 * process, which is quieter than the deploy scripts' version of the same bug
 * and not safer.
 */
export function argvNamesDaemonFor(args, dir) {
  const tokens = String(args).split(/\s+/).filter(Boolean);
  const wanted = ["run", "--dir", dir];
  for (let index = 0; index + wanted.length <= tokens.length; index += 1) {
    if (wanted.every((token, offset) => tokens[index + offset] === token)) return true;
  }
  return false;
}

/**
 * THE COLD PROOF, part one: is this DIRECTORY free of a company?
 *
 * Asked before anything is created, when the directory is all that can be
 * true yet — there is no slug and no company key until genesis mints them, so
 * nothing can serve a session name to look for.
 *
 * Two assertions, and the first is stronger than the three path checks it
 * replaces: `<dir>/.chief` is the COMPLETE footprint, so its absence rules out
 * a store, keys, logs, a rendezvous and a half-built genesis at once.
 */
export function coldDirectoryAssertions({ dir, exists, processTable }) {
  const chief = companyChiefDir(dir);
  return [
    {
      claim: "the directory holds no company",
      observed: exists(chief) ? [chief] : [],
    },
    {
      claim: "no daemon process for this directory",
      observed: processTable
        .filter((entry) => argvNamesDaemonFor(entry.args, dir))
        .map((entry) => `${entry.pid} ${entry.args}`),
    },
  ];
}

/**
 * THE COLD PROOF, part two: has anything already PAINTED this company?
 *
 * Asked after `chief create`, which is the first moment the slug and the
 * company key exist to be served. `create` runs genesis and starts the daemon;
 * it paints no panes — only `attach` and `actuate` do — so a session bearing
 * this company's name here means something else on the box is already
 * actuating it, which is exactly the warm-stack contamination that once
 * produced a 1.77s "measurement" of an unfixed build.
 *
 * Splitting the proof in two is what makes both halves REAL. The old single
 * pass asserted session names before the company existed, which no live
 * session could ever match no matter how warm the box was.
 */
export function coldSessionAssertions({ sessions, slug, key }) {
  const { company, actuator } = sessionNamesFor({ slug, key });
  return [
    {
      claim: "no company tmux session",
      observed: sessions.filter((name) => name === company),
    },
    {
      claim: "no actuator tmux session",
      observed: sessions.filter((name) => name === actuator),
    },
  ];
}

/** The assertions that were violated — an empty list is the cold answer. */
export function violations(assertions) {
  return assertions.filter((entry) => entry.observed.length > 0);
}

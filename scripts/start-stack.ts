#!/usr/bin/env bun
/**
 * `bun scripts/start-stack.ts` — the whole local stack, with every address
 * printed. Invoked by path, not through a package.json target: the root script
 * table is an operator menu, and this is a development entry point the README
 * names directly.
 *
 * #751's definition of done is a browser flow: "the stack boots; a
 * browser can create a company (founder → CEO)". This script is that flow's
 * front door. It starts beacond (discovery), then hands off to turbo for
 * `apps/web`.
 *
 * WHY IT PRINTS: the previous launcher entry point opened a Pi in the current pane and
 * silently started beacond on 127.0.0.1:6969 — a user reported, fairly, "it
 * booted a server but what port? I have no idea what server it's on." A
 * process you started on a port you were never told is indistinguishable from
 * one that failed. Every address this script is responsible for is named on
 * stdout before anything else happens.
 *
 * It does NOT start a company's chiefd. There is one chiefd per company and
 * none exists until a company does; the API resolves each company's chiefd
 * through beacond's registry when it needs one.
 *
 * It DOES start `chief host`, which is a different thing: the per-box
 * lifecycle surface clients call to create, boot and stop a company. It has
 * to be resident for exactly the reason a company's own chiefd cannot serve
 * `create` — the company whose daemon would answer does not exist yet.
 */
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";

import { beacondUrlFromEnvironment, chiefdHostUrlFromEnvironment } from "@chief/chiefing";

/** Where `apps/web` binds (`apps/web/package.json`'s `next dev -p 3000`). */
const WEB_URL = "http://127.0.0.1:3000";

/**
 * Start `chief host` unless one is already answering.
 *
 * Reachability IS the check — a health probe, not a pid file and not a lock:
 * whoever answers on that address is the surface, and a second process that
 * could not bind the port would exit on its own anyway. One user-owned
 * artifact at a known path, never a PATH lookup — a stray same-named binary
 * must not be able to serve this box.
 */
async function ensureChiefdHostRunning(hostUrl: string): Promise<boolean> {
  const answering = await fetch(`${hostUrl}/v1/health`, { signal: AbortSignal.timeout(1000) })
    .then((response) => response.ok)
    .catch(() => false);
  if (answering) return true;

  const home = process.env.HOME?.trim() || homedir();
  const binary = join(home, ".chief", "bin", "chief");
  const child = spawn(binary, ["host"], { stdio: "ignore", detached: true });
  child.on("error", (error) => {
    console.error(`start: could not start 'chief host' from ${binary}: ${error.message}`);
    console.error("start: run 'bun run release' to install the chief binary, then retry.");
  });
  child.unref();
  return false;
}

/**
 * Start `beacond` unless one is already answering.
 *
 * Identical in shape to `ensureChiefdHostRunning` below, and for the same
 * reasons: reachability IS the check (no pid file, no lock), and the binary
 * is the one user-owned artifact at a known path, never a PATH lookup. The
 * launcher-side `beacond-ensure.ts` this replaces was deleted with the rest
 * of the TypeScript lifecycle when `chief|ls|attach|stop|reset` moved
 * into Rust — `chiefd` brings discovery up itself now
 * (`lifecycle::discovery::ensure_running`); this dev entry point is the one
 * caller that starts the stack WITHOUT going through the chiefd binary.
 */
async function ensureBeacondRunning(beacondUrl: string): Promise<boolean> {
  const answering = await fetch(`${beacondUrl}/v1/list`, { signal: AbortSignal.timeout(1000) })
    .then((response) => response.ok || response.status === 404)
    .catch(() => false);
  if (answering) return true;

  const home = process.env.HOME?.trim() || homedir();
  const binary = join(home, ".chief", "bin", "beacond");
  const child = spawn(binary, [], { stdio: "ignore", detached: true });
  child.on("error", (error) => {
    console.error(`start: could not start 'beacond' from ${binary}: ${error.message}`);
    console.error("start: run 'bun run release' to install the beacond binary, then retry.");
  });
  child.unref();
  return false;
}

async function main(): Promise<number> {
  const beacondUrl = beacondUrlFromEnvironment(process.env);
  await ensureBeacondRunning(beacondUrl);
  const chiefdHostUrl = chiefdHostUrlFromEnvironment(process.env);
  await ensureChiefdHostRunning(chiefdHostUrl);

  console.log("");
  console.log("  chief — local stack");
  console.log(`    beacond   ${beacondUrl}      company discovery`);
  console.log(`    chief     ${chiefdHostUrl}      company create/boot/stop`);
  console.log(`    web       http://localhost:3000      open this to create a company`);
  console.log("");
  console.log("  A company's own chiefd starts with the company; there is one");
  console.log("  per company and every client finds it through beacond.");
  console.log("");

  // turbo owns the web from here. Inherit stdio so its logs are the user's
  // logs, and forward the exit code rather than swallowing a failed boot.
  const child = spawn(
    process.execPath,
    ["x", "turbo", "run", "dev", "--concurrency=20", "--filter=@chief/web...", "--output-logs=full"],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        TURBO_UI: "0",
      },
    },
  );
  return await new Promise<number>((resolve) => {
    child.on("exit", (code) => resolve(code ?? 0));
    child.on("error", (error) => {
      console.error(`start: could not run turbo: ${error.message}`);
      resolve(1);
    });
  });
}

if (import.meta.main) {
  main()
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error: unknown) => {
      console.error(`start: ${error instanceof Error ? error.message : String(error)}`);
      process.exitCode = 1;
    });
}

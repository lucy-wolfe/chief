#!/usr/bin/env node
// `bun run web:dev` — the web surface AND the two daemons it cannot work without.
//
// # Why this exists
//
// `apps/web`'s own `dev` script is `next dev`, and turbo's `--filter=@chief/web...`
// pulls in that package's JS dependencies. `beacond` is neither: it is a Rust
// binary, so nothing in the dev graph has ever started it. The web app then
// boots perfectly and renders `beacond unavailable (unreachable) at
// http://127.0.0.1:6969/v1/list` on its first read, which is the honest error
// doing its job — the company registry really is not running.
//
// Reported by the operator on 2026-08-10 as "my web is not working when I run
// bun run web dev". The web app was not broken; the command was incomplete.
//
// # What it does, and what it refuses to do
//
// Idempotent by construction. If something already answers on the beacond URL,
// this reuses it and starts nothing — an operator running their own registry, or
// a second dev session, must never get a second daemon fighting for the port.
//
// It does NOT build. A dev command that silently kicks off a multi-minute cargo
// build is a dev command that looks hung; if a binary is missing this says so
// and names the one command that fixes it.
//
// # Two daemons, not one
//
// beacond (:6969) is the company REGISTRY — which companies exist. `chief host`
// (:8789) is the company LIFECYCLE surface — creating one. Starting only the
// registry got the page rendering and then failed the first button on it with a
// bare `fetch failed`, which is Node's undici wrapper around ECONNREFUSED to
// 8789 with the cause stripped. Verified on a build host: the create request
// really is refused at 8789, and the UI really does surface only "fetch failed".

import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { loadRepoEnv } from './repo-env-lib.mjs'

const HERE = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = resolve(HERE, '..')

// Before anything reads an env var or spawns a child: this wrapper runs under
// node, which does not read `.env` the way bun does, so without this the repo
// `.env` reached neither the daemons started here nor `next dev` under turbo.
loadRepoEnv(REPO_ROOT)

// The one place the default lives is `@chief/chiefing`'s `DEFAULT_BEACOND_URL`;
// this reads the same env var that overrides it, so a developer pointing the web
// app at another registry does not get a local one started underneath them.
const BEACOND_URL = process.env.BEACOND_URL ?? 'http://127.0.0.1:6969'
// `DEFAULT_CHIEFD_HOST_URL` in `@chief/chiefing`, and `CHIEFD_HOST_BIND` in
// `chief-cli/src/host.rs`, are the two definitions this must agree with.
const CHIEFD_HOST_URL = process.env.CHIEFD_HOST_URL ?? 'http://127.0.0.1:8789'

/** Does a registry already answer here? */
async function alreadyRunning(url) {
  try {
    const response = await fetch(new URL('/v1/list', url), {
      signal: AbortSignal.timeout(1500),
    })
    return response.ok
  } catch {
    return false
  }
}

/** Does a daemon already answer here? `probe` is a path known to respond. */
async function answers(url, probe) {
  try {
    const response = await fetch(new URL(probe, url), { signal: AbortSignal.timeout(1500) })
    // Any HTTP answer proves something is listening; a 404 from the wrong path
    // still means the port is not refused, which is the fact being tested.
    return response.status > 0
  } catch {
    return false
  }
}

function beacondBinary() {
  const target = process.env.CARGO_TARGET_DIR
    ? resolve(process.env.CARGO_TARGET_DIR)
    : join(REPO_ROOT, 'apps', 'chiefd', 'target')
  return join(target, 'release', 'beacond')
}

function startBeacond() {
  const binary = beacondBinary()
  if (!existsSync(binary)) {
    console.error(
      `\nbeacond is not built, and the web app cannot list companies without it.\n\n` +
        `  expected: ${binary}\n\n` +
        `Build it once, then re-run this:\n\n` +
        `  cargo build --release --manifest-path apps/chiefd/Cargo.toml --bin beacond\n`,
    )
    process.exit(1)
  }
  const { port } = new URL(BEACOND_URL)
  const child = spawn(binary, [], {
    env: {
      ...process.env,
      BEACOND_BIND: `127.0.0.1:${port || '6969'}`,
      // The watchdog added with beacond's owner-death self-kill (#987/#751).
      // A dev session that is Ctrl-C'd, crashes, or has its terminal closed
      // leaves nothing behind holding port 6969 — which is the exact orphan
      // that made the next `bun run web:dev` fail for a different reason.
      BEACOND_WATCH_PID: String(process.pid),
    },
    stdio: ['ignore', 'inherit', 'inherit'],
  })
  child.on('error', (error) => {
    console.error(`beacond failed to start: ${error.message}`)
    process.exit(1)
  })
  return child
}

function chiefBinary() {
  const target = process.env.CARGO_TARGET_DIR
    ? resolve(process.env.CARGO_TARGET_DIR)
    : join(REPO_ROOT, 'apps', 'chiefd', 'target')
  return join(target, 'release', 'chief')
}

function startChiefdHost() {
  const binary = chiefBinary()
  if (!existsSync(binary)) {
    console.error(
      `\nchief is not built, and the web app cannot create a company without ` +
        `\`chief host\`.\n\n  expected: ${binary}\n\n` +
        `Build it once, then re-run this:\n\n` +
        `  cargo build --release --manifest-path apps/chiefd/Cargo.toml --bin chief\n`,
    )
    process.exit(1)
  }
  // `hostname`, NOT `host`: `URL.host` already carries the port, so `host:port`
  // builds `127.0.0.1:8789:8789` and chiefd cannot resolve it. Caught by
  // starting the thing and reading its log, not by reading this line.
  const { hostname, port } = new URL(CHIEFD_HOST_URL)
  const child = spawn(binary, ['host'], {
    env: {
      ...process.env,
      CHIEFD_HOST_BIND: `${hostname}:${port || '8789'}`,
      BEACOND_URL,
      // TOMBSTONE: `TEAM_LAUNCHER_PI`, pointed at the repo's own
      // `node_modules/.bin/pi`, stood here so a dev create would not fail "Pi
      // is required but no runtime was found". The pin is deleted from the
      // product — chief runs the Pi the operator installed, found on PATH — so
      // dev must not pin one either. A dev environment that resolves a
      // DIFFERENT Pi from the shipped one is the exact confusion the ruling
      // ends, and it would hide a broken PATH from the only people who could
      // notice.
      // The checkout chiefd should treat as its resource root. Named
      // explicitly for dev because the alternative is the `resources/`
      // directory beside the INSTALLED binary, which is a different tree from
      // the one being edited: dev would run the last release's extensions
      // against this checkout's server and report the difference as a product
      // fault. (It used to be defaulted for a related reason — the durable
      // `~/.chief/launcher-root` pointer went stale silently, and a QA box had
      // one naming a deleted directory while every create answered
      // `launcher-root-unusable`. That pointer is gone; the reason to be
      // explicit here is not.)
      ORG_LAUNCHER_ROOT: process.env.ORG_LAUNCHER_ROOT ?? REPO_ROOT,
    },
    stdio: ['ignore', 'inherit', 'inherit'],
  })
  child.on('error', (error) => {
    console.error(`chief host failed to start: ${error.message}`)
    process.exit(1)
  })
  return child
}

if (await alreadyRunning(BEACOND_URL)) {
  console.log(`[dev-web] reusing the beacond already answering at ${BEACOND_URL}`)
} else {
  console.log(`[dev-web] starting beacond at ${BEACOND_URL}`)
  startBeacond()
}

// `chief host` has no watched-pid equivalent, so it is spawned as an ordinary
// CHILD (not detached): it shares this process group and goes down with the
// dev session rather than surviving to hold 8789 against the next run.
let chiefdHost
if (await answers(CHIEFD_HOST_URL, '/v1/company/list')) {
  console.log(`[dev-web] reusing the chief host already answering at ${CHIEFD_HOST_URL}`)
} else {
  console.log(`[dev-web] starting chief host at ${CHIEFD_HOST_URL}`)
  chiefdHost = startChiefdHost()
}

// The third thing a company create needs, and the only one this script cannot
// supply: the model route its CEO boots on. That route is not configuration —
// it is whatever this box's Pi is on — so the check is whether Pi has been
// pointed at a model at all. chiefd refuses genesis without one, with a clear
// message, but at first-click, after the operator has typed a name and a
// purpose. Said here instead, at start, because it is a fact about the box and
// not about the button.
const piSettings = join(
  process.env.PI_SOURCE_AGENT_DIR?.trim() || join(homedir(), '.pi', 'agent'),
  'settings.json',
)
if (!existsSync(piSettings)) {
  console.warn(
    `[dev-web] ${piSettings} does not exist — creating a company will fail.\n` +
      `[dev-web] Choose a model in Pi; that writes the defaultProvider and defaultModel` +
      ` a new company inherits.`,
  )
}

const turbo = spawn(
  'turbo',
  [
    'run',
    'dev',
    '--concurrency=20',
    '--filter=@chief/web...',
    '--output-logs=full',
  ],
  { cwd: REPO_ROOT, env: { ...process.env, TURBO_UI: '0' }, stdio: 'inherit' },
)

// Forward the signals a developer actually sends, so Ctrl-C stops the dev
// server rather than orphaning it under this wrapper. beacond needs no
// forwarding: it is watching this process's pid and exits on its own.
const FORWARDED_SIGNALS = /** @type {NodeJS.Signals[]} */ (['SIGINT', 'SIGTERM'])
for (const signal of FORWARDED_SIGNALS) {
  process.on(signal, () => {
    turbo.kill(signal)
    chiefdHost?.kill(signal)
  })
}
turbo.on('exit', (code, signal) => {
  chiefdHost?.kill('SIGTERM')
  process.exit(signal ? 1 : (code ?? 0))
})

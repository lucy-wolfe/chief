import { readFileSync } from 'node:fs'
import { join } from 'node:path'

import { isNullish } from '../Nullish.js'
import type { DaemonRendezvous } from '../types/Discovery.js'

/**
 * How a process standing INSIDE a company directory finds that company's
 * daemon and its own company key.
 *
 * The Rust half is `host_primitives::rendezvous` (written by `chiefd-daemon`,
 * read by `chief-cli`); this is the third reader, for the Pi extensions that
 * run in a pane whose cwd is the company directory. A directory already knows
 * where its own daemon is, so nothing here asks beacond: the registry answers
 * the box-wide question ("what is running anywhere"), never "where is mine".
 *
 * # The file is a POINTER, never authority
 *
 * Every durable fact about a company is a row in `<dir>/.chief/db/chief.db`.
 * This file says only "a daemon for this directory was last seen at this URL
 * under this pid". A stale file is the ordinary case after a reboot — the
 * caller still has to reach the URL, and a dead one fails the way any dead
 * address does. It lives under `.chief/run/`, which is disposable by
 * construction.
 */

/** The file name, under `<dir>/.chief/run/`. Byte-identical to Rust's
 * `host_primitives::rendezvous::RENDEZVOUS_FILENAME`. */
export const RENDEZVOUS_FILENAME = 'daemon.json'

/** Where the rendezvous lives for one company directory. Must stay
 * byte-identical to `host_primitives::rendezvous::rendezvous_path`. */
export function rendezvousPath(dir: string): string {
  return join(dir, '.chief', 'run', RENDEZVOUS_FILENAME)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !isNullish(value) && typeof value === 'object' && !Array.isArray(value)
}

function malformed(detail: string): never {
  throw new Error(`malformed daemon rendezvous: ${detail}`)
}

/**
 * Parse one rendezvous body, proving it describes `dir`.
 *
 * **The directory check is the point, not a formality.** `.chief/` sits inside
 * the company directory, so copying a project copies its rendezvous — and a
 * copy names the ORIGINAL directory. Binding it would point one company's
 * client at another company's daemon, which is exactly the split-brain the
 * composite key existed to prevent.
 *
 * # AN UNKNOWN FIELD IS IGNORED, and that reversal is an outage's worth of
 * evidence
 *
 * This used to REFUSE any field it did not model, mirroring the Rust side's
 * `deny_unknown_fields`, on the reasoning that two programs which ship
 * together disagreeing about a field is a version skew worth failing on.
 *
 * **Measured, live, 2026-08-26: it is not.** The daemon began publishing a
 * `build` field — which binary it is running, so a stale daemon can be
 * replaced. Additive, backward compatible, and correct on the writer's side.
 * Every person in the operator's company then died at start-up with
 * `Failed to load extension … unknown field "build" — this daemon is a
 * different build`, because THIS reader refused the record it was handed.
 * An additive change to a writer became a cross-language outage purely
 * because a reader was strict.
 *
 * **A rendezvous is a RECORD somebody else writes, and forward compatibility
 * is the correct posture for reading one.** What this parser owes its callers
 * is that the fields it USES are present and well-formed — which it still
 * checks, every one, below. What it does not owe anybody is an opinion about
 * fields it has never heard of. Strictness there makes every future field
 * addition an outage, and the outage lands on the reader that is furthest from
 * the change.
 *
 * The sentence that reported this as `this daemon is a different build` is
 * gone with the check. It named a skew that was not occurring and sent every
 * reader after a version mismatch that did not exist. An unparsable rendezvous
 * and a genuine build mismatch are different findings with different remedies,
 * and this parser is not the thing that detects the second.
 */
export function parseDaemonRendezvous(value: unknown, dir: string): DaemonRendezvous {
  if (!isRecord(value)) malformed('not an object')
  const recordedDir = value.dir
  const key = value.key
  const url = value.url
  const pid = value.pid
  if (typeof recordedDir !== 'string' || recordedDir.length === 0) {
    malformed('dir must be a non-empty string')
  }
  if (typeof key !== 'string' || !/^[0-9a-f]{12}$/.test(key)) {
    malformed('key must be twelve lowercase hex characters')
  }
  if (typeof url !== 'string' || url.length === 0) malformed('url must be a non-empty string')
  if (typeof pid !== 'number' || !Number.isInteger(pid) || pid < 1) {
    malformed('pid must be a positive integer')
  }
  if (recordedDir !== dir) {
    malformed(`it describes "${recordedDir}", not "${dir}" — a copied rendezvous names its origin`)
  }
  return { dir: recordedDir, key, url, pid }
}

/**
 * Read `<dir>/.chief/run/daemon.json`, or `undefined` when no daemon has
 * published there.
 *
 * An ABSENT file is not an error: a company that has never been started, or
 * one whose disposable `run/` folder was deleted, legitimately has none, and
 * the caller's answer is "boot it". A file that is present but unreadable as
 * this contract THROWS — that is a daemon disagreeing with its own client,
 * which no caller can recover from by guessing.
 */
export function readDaemonRendezvous(dir: string): DaemonRendezvous | undefined {
  let text: string
  try {
    text = readFileSync(rendezvousPath(dir), 'utf8')
  } catch {
    return undefined
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch (cause) {
    throw new Error(`malformed daemon rendezvous: ${rendezvousPath(dir)} is not JSON`, { cause })
  }
  return parseDaemonRendezvous(parsed, dir)
}

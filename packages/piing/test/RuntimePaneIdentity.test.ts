/**
 * TMUX OWNS TWO OF THESE NAMES, AND THIS FILE IS WHY THEY CANNOT BE RENAMED.
 *
 * `packages/piing/test/runtime/LauncherBunResolution.test.ts` was a sibling of
 * this file and is deleted: it pinned `launcherRuntimeBinary`, which chose the
 * launcher's stamped `TEAM_LAUNCHER_BUN` over a PATH-resolved `"bun"` so a
 * pane's launcher SUBPROCESS did not die with `spawn bun ENOENT`. #751/G9
 * deleted the subprocess, so nothing resolves a bun to spawn any more.
 *
 * #751/P9 swept `tmux` -> `runtime` across the tree to get the tmux vocabulary
 * out of the backend crates. Two of the strings it caught in this extension are
 * not vocabulary at all — they are names owned by tmux itself, and renaming
 * them does not rename anything, it just stops the code finding what it is
 * looking for:
 *
 *   1. `spawnSync("tmux", ["-L", …, "list-panes", …])` — argv[0] is the name of
 *      a REAL EXECUTABLE. Rewritten to `"runtime"` it becomes an ENOENT that
 *      `authoritativeRuntimePane` reports, correctly for its own contract, as
 *      "no pane found" — so the pane-recovery tier silently stops recovering
 *      anything and nothing anywhere goes red.
 *   2. `TMUX_PANE` — exported by TMUX into every pane's environment. Rewritten
 *      to `RUNTIME_PANE` there is no writer for it in the universe, so the raw
 *      tier of the identity ladder in `readOrganizationRuntimeContext` is dead
 *      and every pane falls through to the wrapper's preserved token.
 *
 * BOTH FAILURES ARE SILENT BY CONSTRUCTION, which is the entire reason they are
 * pinned here rather than left to a suite that happens to touch them: each one
 * degrades to a legitimate-looking "not found" answer that the surrounding code
 * is designed to tolerate. Nothing throws, nothing logs, and the identity ladder
 * just gets shorter.
 *
 * The assertions below are deliberately about the LITERAL STRINGS, not about
 * behaviour a rename would preserve — a test that only checked "a pane id comes
 * back" passes against a fake `run` regardless of which binary the real code
 * would have spawned, which is exactly how the regression got in.
 */
import type { spawnSync } from 'node:child_process'

import {
  authoritativeRuntimePane,
  readOrganizationRuntimeContext
} from '@test-assets/organization-intercom'
import { describe, expect, test } from 'vitest'

interface RecordedCall {
  command: string
  args: readonly string[]
}

/**
 * A `spawnSync` stand-in that records every argv it is handed and replies only
 * to the command names it was given. Replying by NAME is the point: a fake that
 * answered any command would make the argv[0] assertions below vacuous.
 */
function recordingRun(replies: Readonly<Record<string, string>>): {
  run: typeof spawnSync
  calls: RecordedCall[]
} {
  const calls: RecordedCall[] = []
  function fake(command: string, args: readonly string[]): unknown {
    calls.push({ command, args })
    return { status: 0, stdout: replies[command] ?? '', stderr: '' }
  }
  /* eslint-disable @typescript-eslint/consistent-type-assertions */
  // `spawnSync` is a large overloaded signature whose `pid`/`output`/`signal`
  // fields this code path never reads; structurally satisfying all of it would
  // be a second, always-stale copy of a shape the function under test ignores.
  const run = fake as unknown as typeof spawnSync
  /* eslint-enable @typescript-eslint/consistent-type-assertions */
  return { run, calls }
}

const BASE_ENVIRONMENT: Record<string, string | undefined> = {
  ORG_LAUNCHER_IDENTITY_DIR: '/orgs/acme/.chief',
  ORG_LAUNCHER_ORG_DIR: '/orgs/acme',
  ORG_LAUNCHER_ORGANIZATION: 'acme',
  ORG_LAUNCHER_ROOT: '/repo',
  ORG_LAUNCHER_PERSON: 'ceo',
  ORG_LAUNCHER_RUNTIME_SOCKET: 'acme',
  ORG_LAUNCHER_RUNTIME_SESSION: 'org-acme'
}

describe('the pane-identity names tmux owns, not this repo', () => {
  test('authoritativeRuntimePane spawns the tmux BINARY: argv[0] is "tmux", not a renamed synonym', () => {
    // `%7` is this process's own pane because the fake reports its pid there.
    const { run, calls } = recordingRun({ tmux: `%7\t${process.pid}\n` })

    expect(authoritativeRuntimePane('acme', process.pid, run)).toBe('%7')

    const spawned = calls.at(0)
    expect(spawned?.command).toBe('tmux')
    // The flags are tmux's own, which is the proof argv[0] has to be tmux: no
    // other program answers `-L <socket> list-panes -F '#{pane_id}'`.
    expect(spawned?.args).toStrictEqual([
      '-L',
      'acme',
      'list-panes',
      '-a',
      '-F',
      '#{pane_id}\t#{pane_pid}'
    ])
  })

  test('a renamed binary is indistinguishable from "no pane" — the shape of the silence, asserted', () => {
    // The negative control. The fake answers only to `runtime`, so if the code
    // under test ever spawns that name it gets a pane back; spawning `tmux`
    // gets empty stdout, the same `undefined` a genuinely-unfound pane
    // produces. Nothing throws either way, which is the whole danger.
    const { run, calls } = recordingRun({ runtime: `%7\t${process.pid}\n` })

    expect(authoritativeRuntimePane('acme', process.pid, run)).toBe(undefined)
    expect(calls.at(0)?.command).toBe('tmux')
  })

  test('readOrganizationRuntimeContext reads the raw pane from TMUX_PANE, the variable tmux itself exports', () => {
    const context = readOrganizationRuntimeContext({ ...BASE_ENVIRONMENT, TMUX_PANE: '%3' })
    expect(context.runtimePane).toBe('%3')
  })

  test('RUNTIME_PANE is not a synonym: nothing sets it, so it must not satisfy the raw tier', () => {
    // With only RUNTIME_PANE present and no preserved token the ladder has to
    // fall through rather than accept it. The socket is dropped here so the
    // third tier cannot shell out to a real tmux from a unit test.
    const context = readOrganizationRuntimeContext({
      ...BASE_ENVIRONMENT,
      ORG_LAUNCHER_RUNTIME_SOCKET: undefined,
      ORG_LAUNCHER_RUNTIME_SESSION: undefined,
      RUNTIME_PANE: '%3'
    })
    expect(context.runtimePane).toBe(undefined)
  })

  test('the raw TMUX_PANE tier outranks the wrapper token, and a malformed one is refused rather than downgraded', () => {
    const both = readOrganizationRuntimeContext({
      ...BASE_ENVIRONMENT,
      TMUX_PANE: '%3',
      ORG_LAUNCHER_PANE_ID: '%9'
    })
    expect(both.runtimePane).toBe('%3')

    expect(() =>
      readOrganizationRuntimeContext({ ...BASE_ENVIRONMENT, TMUX_PANE: 'not-a-pane' })
    ).toThrow(/TMUX_PANE/)
  })

  // THE WRITE HALF IS DELETED, AND THAT IS WHY THERE IS NO SIXTH TEST HERE.
  //
  // A sixth case pinned `launcherWorkerActionEnvironment(context, env)` — the
  // fenced CLI child's environment — handing `TMUX_PANE` back under tmux's own
  // spelling and never also setting `RUNTIME_PANE`. That function existed only
  // to build the environment for a launcher SUBPROCESS, and #751/G9 deleted the
  // whole subprocess transport, so there is no child to hand a pane token to
  // any more. The test went with its subject rather than being rewritten
  // against a fake: an assertion about how a deleted function spells a variable
  // proves nothing about the product.
  //
  // The rule the deleted case guarded is unchanged and still guarded, by the
  // four cases above: `TMUX_PANE` is tmux's name, this process READS it, and
  // renaming the read is a silent identity-ladder deletion. Only the hand-off
  // to a second process is gone.
})

// Where a person's Pi transcripts live — the one derivation of a fact that
// used to be written out twice.
//
// chiefd materializes each person under `<orgs>/<slug>/people/<personId>/` with
// two siblings: `workspace/` (the agent's cwd, which the launch profile
// carries) and `pi-home/` (sessions, which it does not). The profile names the
// workspace and leaves the host to find the home beside it.
//
// Two modules needed that step — the harness and the extension host — and each
// had its own copy. One then disagreed with chiefd about where a transcript
// belongs, and every fresh person failed their first turn. These tests pin the
// shape both callers now share, because a second derivation of one fact always
// eventually produces that failure again.
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

import { sessionsDir } from '@/server/PiHome'

const WORKSPACE = join('/orgs', 'acme', 'people', 'person-ceo', 'workspace')
const PERSON_ROOT = join('/orgs', 'acme', 'people', 'person-ceo')

describe('sessionsDir', () => {
  it('points at the directory chiefd itself scans for transcripts', () => {
    // `resource_catalog::latest_session` reads `<pi-home>/sessions/` and
    // resumes the newest `.jsonl` in it. The old fallback wrote
    // `<cwd>/session.jsonl`, which chiefd never looks at — so the web held one
    // conversation and the CLI pane another with the same agent, each unaware
    // of the other. This path is the reason there is only one.
    expect(sessionsDir(WORKSPACE)).toBe(join(PERSON_ROOT, 'pi-home', 'sessions'))
  })

  it('finds the home BESIDE the workspace, not inside it', () => {
    // Inside the workspace it would be part of the agent's own working tree —
    // committed, listed by its own tools, and wiped by anything that resets a
    // checkout. It is a sibling because chiefd makes it one.
    expect(sessionsDir(WORKSPACE).startsWith(join(PERSON_ROOT, 'pi-home'))).toBe(true)
  })

  it('keeps two people’s transcripts apart', () => {
    const ada = sessionsDir(join('/orgs', 'acme', 'people', 'ada', 'workspace'))
    const bob = sessionsDir(join('/orgs', 'acme', 'people', 'bob', 'workspace'))

    expect(ada).not.toBe(bob)
  })
})

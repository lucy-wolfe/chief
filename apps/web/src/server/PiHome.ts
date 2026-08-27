/**
 * Where a person's Pi transcripts live, derived once.
 *
 * chiefd materializes each person under `<orgs>/<slug>/people/<personId>/`
 * with two siblings: `workspace/` (the agent's cwd, which the launch profile
 * carries) and `pi-home/` (sessions), which it does not. The profile names the
 * workspace and leaves the host to find the home beside it.
 *
 * Two modules need that step — the harness and the extension host — and they
 * had it written out twice. One of them then disagreed with chiefd about where
 * a transcript belongs and every fresh person failed their first turn (see
 * `AgentHost`), which is the failure a second derivation of one fact always
 * eventually produces.
 */
import { join } from 'node:path'

/**
 * The directory chiefd scans for a person's transcripts.
 *
 * `resource_catalog::latest_session` reads `<pi-home>/sessions/` and resumes
 * the newest `.jsonl` in it. A transcript written anywhere else is invisible
 * to chiefd and to the CLI pane, so the two would hold separate conversations
 * with the same agent.
 */
export function sessionsDir(cwd: string): string {
  return join(cwd, '..', 'pi-home', 'sessions')
}

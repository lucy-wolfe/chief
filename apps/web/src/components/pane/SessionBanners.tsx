import type { ReactElement } from 'react'

import type { AgentConversationRuntime } from '@/types/Conversation'
import type { PersonHostEvent, PersonHostState, SseChannelState } from '@/types/Sse'

interface SessionBannersProps {
  readOnly: boolean
  host: PersonHostEvent | undefined
  hostState: PersonHostState | undefined
  channel: SseChannelState
  runtime: AgentConversationRuntime
}

function banner(text: string, key: string): ReactElement {
  return (
    <p key={key} role="status" style={{ margin: 0, padding: '3px 6px' }}>
      {text}
    </p>
  )
}

/** Distinct host/channel/compaction/retry availability notices for one pane. */
export function SessionBanners({
  readOnly,
  host,
  hostState,
  channel,
  runtime
}: SessionBannersProps): ReactElement {
  const banners: ReactElement[] = []
  if (readOnly) banners.push(banner('Visible via CLI tmux; this pane is read-only.', 'tmux'))
  if (hostState === 'starting') banners.push(banner('Agent host is starting.', 'starting'))
  // A dormant person is the RESTING state of every non-CEO pane, not a fault:
  // a company at rest runs only its CEO.
  //
  // THE "START AGENT" BUTTON IS GONE. It posted to `…/people/:id/start`, a
  // route this server has never served, so the one action the banner offered
  // answered 404 and replaced the resting notice with an error. And no handler
  // is owed here: who is up is chiefd's roster decision — it converges the
  // host from the durable roster — so a browser waking one person out of band
  // would be a second opinion about the roster. Saying "dormant" plainly is
  // honest; offering a button that cannot wake anybody is not.
  if (hostState === 'stopped') banners.push(banner('Agent is dormant.', 'stopped'))
  if (hostState === 'exited') {
    const exitCode = typeof host?.exitCode === 'number' ? ` (exit ${host.exitCode})` : ''
    banners.push(banner(`Agent host exited${exitCode}.`, 'exited'))
  }
  if (channel === 'dead') banners.push(banner('Reconnecting session stream…', 'dead'))
  if (runtime.isCompacting) banners.push(banner('Compacting session…', 'compacting'))
  if (runtime.isRetrying) banners.push(banner('Retrying agent response…', 'retrying'))
  if (runtime.queuedMessages > 0) {
    banners.push(banner(`${runtime.queuedMessages} queued message(s).`, 'queued'))
  }
  if (runtime.isSettled && hostState === 'running') banners.push(banner('Agent idle.', 'idle'))

  return <>{banners}</>
}

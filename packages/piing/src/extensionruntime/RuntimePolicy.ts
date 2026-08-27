const ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS = 4 * 60

export function organizationForegroundResponsivenessContract(): string {
  const timeoutMinutes = ORGANIZATION_FOREGROUND_BASH_TIMEOUT_SECONDS / 60
  const bounded =
    '- Keep foreground commands bounded and interactive: managed Bash receives a ' +
    `${timeoutMinutes}-minute maximum so queued organization mail can re-enter this Pi session.`
  const durable =
    '- Never hold a foreground tool open to sleep until a future time, poll indefinitely, ' +
    'tail forever, or host a daemon/server. Arm a durable reminder with `org_create_reminder` ' +
    'for future work; ' +
    'use a truly detached process with redirected stdio and an explicit supervisor only when ' +
    'a persistent process is the actual deliverable.'
  return `## Foreground responsiveness\n\n${bounded}\n${durable}`
}

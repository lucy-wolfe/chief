'use client'

import type { ReactElement } from 'react'

import type { CompanyLifecycleFailure, CompanyLifecycleSuccess } from '@/types/Companies'
import type { LifecyclePhaseFrame } from '@/types/Sse'

interface BootPhaseConsoleProps {
  readonly label: string
  readonly phases: readonly LifecyclePhaseFrame[]
  readonly terminal?: CompanyLifecycleSuccess
  readonly failure?: CompanyLifecycleFailure
  readonly running: boolean
  readonly onRetry?: () => void
}

/** Lifecycle phase text is intentionally displayed as received.  The browser
 * does not own a phase vocabulary and therefore cannot translate one. */
export function BootPhaseConsole({
  label,
  phases,
  terminal,
  failure,
  running,
  onRetry
}: BootPhaseConsoleProps): ReactElement {
  return (
    <section
      aria-live="polite"
      data-lifecycle-console="true"
      style={{
        background: 'var(--chief-status-bg)',
        fontFamily: "ui-monospace, 'SF Mono', Menlo, Consolas, monospace",
        padding: '8px'
      }}
    >
      <h2>{label}</h2>
      <output>
        {phases.map((frame, index) => (
          <div
            data-lifecycle-phase={frame.phase}
            key={`${index}:${frame.phase}:${frame.detail ?? ''}`}
          >
            {frame.phase} — {frame.detail ?? ''}
          </div>
        ))}
      </output>
      {running ? <p>Waiting for lifecycle result…</p> : null}
      {terminal ? <p>Finished: {terminal.slug}</p> : null}
      {failure ? (
        <div>
          <p role="alert">
            {failure.code}: {failure.detail}
          </p>
          {onRetry ? (
            <button onClick={onRetry} type="button">
              Retry
            </button>
          ) : null}
        </div>
      ) : null}
    </section>
  )
}

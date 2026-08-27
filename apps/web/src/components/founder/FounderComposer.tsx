'use client'

/**
 * The Founder's composer.
 *
 * # Why this is not `PaneComposer`
 *
 * That composer is built for a STREAMING pane: it offers `steer` and
 * `followUp` while a turn runs, and its Abort button is enabled exactly when a
 * turn is streaming and nothing is being submitted. Founder's turn IS the
 * submission — `say` awaits the whole turn — so on that composer the mode
 * selector would offer two queues nobody drains, and Abort would be disabled
 * for precisely the whole time there is something to abort.
 *
 * Reusing it would therefore have shipped two controls that do nothing. The
 * conversation ITSELF is not duplicated: `ConversationView` renders both panes
 * from the same rows.
 */
import { type FormEvent, type ReactElement, useState } from 'react'

interface FounderComposerProps {
  /** A turn is in flight. */
  readonly pending: boolean
  readonly onSend: (text: string) => Promise<void>
  readonly onAbort: () => Promise<void>
}

export function FounderComposer({ pending, onSend, onAbort }: FounderComposerProps): ReactElement {
  const [text, setText] = useState('')

  function submit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault()
    const trimmed = text.trim()
    if (pending || trimmed === '') return
    // Cleared before the turn rather than after: a Founder turn can run for
    // minutes when it launches, and leaving the sent message in the box makes
    // it look unsent.
    setText('')
    void onSend(trimmed)
  }

  return (
    <form className="chief-founder-composer" onSubmit={submit}>
      <textarea
        aria-label="Message Founder"
        disabled={pending}
        onChange={(event): void => setText(event.target.value)}
        placeholder="Name the company and its purpose"
        value={text}
      />
      <div className="chief-form-actions">
        <button
          className="chief-button chief-button--primary"
          disabled={pending || text.trim() === ''}
          type="submit"
        >
          {pending ? 'Founder is working…' : 'Send'}
        </button>
        <button
          aria-label="Abort Founder"
          className="chief-button"
          disabled={!pending}
          onClick={(): void => void onAbort()}
          type="button"
        >
          Stop
        </button>
      </div>
    </form>
  )
}

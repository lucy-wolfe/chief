'use client'

import {
  type FormEvent,
  type KeyboardEvent,
  type ReactElement,
  useEffect,
  useRef,
  useState
} from 'react'

import type { SayMode } from '@/types/ChiefApi'

interface PaneComposerProps {
  disabled: boolean
  isStreaming: boolean
  onSend(message: string, mode: SayMode): Promise<void>
  onAbort(): Promise<void>
}

function sayMode(value: string): SayMode {
  switch (value) {
    case 'steer':
      return 'steer'
    case 'followUp':
      return 'followUp'
    default:
      return 'prompt'
  }
}

/** In-memory-only message composer; stream echo remains the rendered source. */
export function PaneComposer({
  disabled,
  isStreaming,
  onSend,
  onAbort
}: PaneComposerProps): ReactElement {
  const [message, setMessage] = useState('')
  const [mode, setMode] = useState<SayMode>('prompt')
  const [submitting, setSubmitting] = useState(false)
  const submittingRef = useRef(false)
  const abortButtonRef = useRef<HTMLButtonElement | null>(null)

  useEffect(() => {
    if (!isStreaming) setMode('prompt')
  }, [isStreaming])

  async function submit(): Promise<void> {
    const trimmed = message.trim()
    if (disabled || submittingRef.current || trimmed.length === 0) return
    submittingRef.current = true
    setMessage('')
    setSubmitting(true)
    try {
      await onSend(trimmed, mode)
    } finally {
      submittingRef.current = false
      setSubmitting(false)
    }
  }

  function handleSubmit(event: FormEvent<HTMLFormElement>): void {
    event.preventDefault()
    void submit()
  }

  function handleKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key !== 'Escape') return
    event.preventDefault()
    abortButtonRef.current?.focus()
  }

  return (
    <form onSubmit={handleSubmit} style={{ display: 'flex', gap: '4px', padding: '4px' }}>
      <textarea
        aria-label="Message agent"
        disabled={disabled || submitting}
        onChange={(event): void => setMessage(event.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="Message agent"
        value={message}
        style={{ flex: 1, minHeight: '3rem' }}
      />
      {isStreaming ? (
        <select
          aria-label="Message mode"
          disabled={disabled || submitting}
          onChange={(event): void => setMode(sayMode(event.target.value))}
          value={mode}
        >
          <option value="prompt">Prompt</option>
          <option value="steer">Steer</option>
          <option value="followUp">Follow up</option>
        </select>
      ) : null}
      <button disabled={disabled || submitting || message.trim().length === 0} type="submit">
        Send
      </button>
      <button
        aria-label="Abort agent"
        disabled={disabled || submitting || !isStreaming}
        onClick={(): void => void onAbort()}
        ref={abortButtonRef}
        type="button"
      >
        Abort
      </button>
    </form>
  )
}

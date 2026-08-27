/** Pure incremental parser for the browser's three app-API SSE families. */
import type { SseFrame } from '@/types/Sse'

interface PendingFrame {
  id: string | undefined
  event: string | undefined
  data: string[]
}

function emptyPendingFrame(): PendingFrame {
  return { id: undefined, event: undefined, data: [] }
}

function frameFromPending(pending: PendingFrame): SseFrame | undefined {
  if (
    typeof pending.id === 'undefined' &&
    typeof pending.event === 'undefined' &&
    pending.data.length === 0
  ) {
    return undefined
  }
  const frame: SseFrame = {}
  if (typeof pending.id !== 'undefined') frame.id = pending.id
  if (typeof pending.event !== 'undefined') frame.event = pending.event
  if (pending.data.length > 0) frame.data = pending.data.join('\n')
  return frame
}

/**
 * Incrementally accepts decoded text rather than assuming frame-aligned
 * network chunks. Comments become their own proof-of-life frames; unknown
 * SSE fields are intentionally ignored for forward compatibility.
 */
export function createSseFrameParser(): {
  push(chunk: string): SseFrame[]
  reset(): void
} {
  let buffer = ''
  let pending = emptyPendingFrame()

  function handleLine(line: string, frames: SseFrame[]): void {
    if (line.length === 0) {
      const frame = frameFromPending(pending)
      pending = emptyPendingFrame()
      if (frame) frames.push(frame)
      return
    }
    if (line.startsWith(':')) {
      frames.push({ comment: true })
      return
    }

    const colon = line.indexOf(':')
    const field = colon === -1 ? line : line.slice(0, colon)
    const rawValue = colon === -1 ? '' : line.slice(colon + 1)
    const value = rawValue.startsWith(' ') ? rawValue.slice(1) : rawValue
    switch (field) {
      case 'id':
        pending.id = value
        break
      case 'event':
        pending.event = value
        break
      case 'data':
        pending.data.push(value)
        break
      default:
        break
    }
  }

  return {
    push(chunk: string): SseFrame[] {
      buffer += chunk
      const frames: SseFrame[] = []
      let lineStart = 0
      for (let index = 0; index < buffer.length; index += 1) {
        if (buffer[index] !== '\n') continue
        let line = buffer.slice(lineStart, index)
        if (line.endsWith('\r')) line = line.slice(0, -1)
        handleLine(line, frames)
        lineStart = index + 1
      }
      buffer = buffer.slice(lineStart)
      return frames
    },
    reset(): void {
      buffer = ''
      pending = emptyPendingFrame()
    }
  }
}

/** Pure chiefing-parity exponential backoff with a hard cap. */
export function computeBackoffDelayMs(attempt: number, initialMs: number, maxMs: number): number {
  return Math.min(initialMs * 2 ** attempt, maxMs)
}

/**
 * The one SSE frame decoder in chiefing.
 *
 * chiefd now pushes two different streams at TypeScript — `/v1/docs/watch`'s
 * long-lived document change feed and `chief host`'s per-operation company
 * lifecycle stream — and both are `text/event-stream`, which means both need
 * the same three things: accumulate bytes, split on blank lines, and read the
 * `event:`/`data:`/`id:` fields out of each block. That is not two problems, so
 * it is not two implementations: `SseWatcher` and `CompanyLifecycleClient` both
 * feed this decoder and differ only in what they do with a finished frame.
 *
 * The split matters beyond deduplication. Frame decoding is a pure function of
 * the bytes so far; everything a watcher does around it — reconnect backoff,
 * heartbeat deadlines, connection fencing, per-store coalescing — is state that
 * has nothing to do with parsing. Keeping them together is what made the
 * parsing untestable without also standing up a connection.
 *
 * Imports ONLY relative siblings + builtins (no `@/` alias, no package
 * specifier): `packages/chiefing/src/extensionruntime` republishes this file's
 * dependency closure into Pi homes that have no `node_modules`, and
 * `SseWatcher` — which is in that closure — imports this module.
 */

import { isNullish } from '../Nullish.js'
import type { SseFrame } from '../types/Watch.js'

/**
 * Incremental SSE decoder.
 *
 * `push(chunk)` returns the frames that chunk completed — usually zero or one,
 * more when a slow reader receives several at once. Nothing is buffered beyond
 * the trailing partial line, so a stream that is quiet for an hour costs one
 * short string.
 */
export class SseFrameDecoder {
  private buffer = ''
  private event: string | undefined
  private id: string | undefined
  private data: string[] = []
  /** True once any field line has been seen for the frame being assembled.
   * Distinguishes "a real frame whose data happens to be empty" from "the
   * blank line that terminated the previous frame", which a `data`-length
   * check alone cannot do. */
  private started = false

  /** Feed decoded text; get back whatever frames it completed. */
  push(chunk: string): SseFrame[] {
    this.buffer += chunk
    const frames: SseFrame[] = []
    // Split on both line endings: the spec permits CRLF, and a server behind a
    // proxy that rewrites them is not a case worth failing on.
    const lines = this.buffer.split(/\r?\n/)
    // The last element is either a partial line or '' when the chunk ended on a
    // newline; either way it is not yet a complete line.
    this.buffer = lines.pop() ?? ''
    for (const line of lines) {
      const frame = this.line(line)
      if (frame) frames.push(frame)
    }
    return frames
  }

  /**
   * Flush a frame the stream ended without a trailing blank line after.
   *
   * A well-behaved server always sends one, so this is normally a no-op. It
   * exists because a per-operation stream — one that closes as soon as its
   * terminal frame is written — is exactly where a missing final newline would
   * silently swallow the one frame the caller was waiting for.
   */
  flush(): SseFrame | undefined {
    if (this.buffer.length > 0) {
      const line = this.buffer
      this.buffer = ''
      const frame = this.line(line)
      if (frame) return frame
    }
    return this.complete()
  }

  /** Consume one complete line. A blank line terminates the current frame. */
  private line(line: string): SseFrame | undefined {
    if (line === '') return this.complete()
    // A comment (`:hb`) is a liveness heartbeat and carries no fields. It is
    // reported as a frame with the reserved `comment` type rather than dropped
    // here: a caller's channel-liveness logic needs to see it, and inventing a
    // second callback for "something arrived" would be the same information
    // twice.
    if (line.startsWith(':')) {
      return { event: 'comment', data: line.slice(1) }
    }
    const colon = line.indexOf(':')
    const field = colon === -1 ? line : line.slice(0, colon)
    const raw = colon === -1 ? '' : line.slice(colon + 1)
    // Exactly one leading space is part of the framing, not the value.
    const value = raw.startsWith(' ') ? raw.slice(1) : raw
    if (field === 'event') {
      this.event = value
      this.started = true
    } else if (field === 'data') {
      this.data.push(value)
      this.started = true
    } else if (field === 'id') {
      this.id = value
      this.started = true
    }
    // Unknown fields (`retry:`, anything future) are ignored rather than
    // rejected — forward compatibility is the whole reason the wire is
    // field-based.
    return undefined
  }

  /** Emit the frame under assembly, if there is one, and reset. */
  private complete(): SseFrame | undefined {
    if (!this.started) return undefined
    const frame: SseFrame = {
      event: this.event ?? 'message',
      data: this.data.join('\n'),
      ...(isNullish(this.id) ? {} : { id: this.id })
    }
    this.event = undefined
    this.id = undefined
    this.data = []
    this.started = false
    return frame
  }
}

/**
 * Read an async iterable of decoded text chunks as a stream of SSE frames.
 *
 * The generator form is what lets a caller write `for await (const frame of …)`
 * and get ordinary `break`/`return` semantics: breaking out closes the source
 * through the iterator protocol, which is how a lifecycle stream is abandoned
 * without a separate cancellation channel.
 */
export async function* readSseFrames(
  chunks: AsyncIterable<string>
): AsyncGenerator<SseFrame, void, unknown> {
  const decoder = new SseFrameDecoder()
  for await (const chunk of chunks) {
    for (const frame of decoder.push(chunk)) yield frame
  }
  const last = decoder.flush()
  if (last) yield last
}

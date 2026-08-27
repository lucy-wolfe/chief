'use client'

/**
 * The Founder conversation, in the browser.
 *
 * # Why this is not `UseAgentConversation`
 *
 * That hook hydrates a transcript and then folds a live SSE stream of session
 * events into it, because a company's person is driven by chiefd and things
 * happen to them that nobody in this browser asked for — a mailbox delivery, a
 * model change, another client's turn. None of that is true of Founder: it is
 * a singleton with one operator, nothing drives it, and the only thing that
 * ever changes its transcript is a request made from this page.
 *
 * So there is no stream, and this hook re-reads the transcript when a turn
 * ENDS. That is not polling and mandate 1 is intact: the read is caused by an
 * event (a turn completing), not by a clock, and nothing here asks again for
 * state nobody said had changed.
 *
 * # Why the reply is not simply appended
 *
 * `say` returns the assistant's words, and rendering only those would silently
 * drop the launch tool's own result — the one row that says a company was
 * created. Re-reading gives the same rows the server has, through
 * `rowsFromTranscript`, which is the reader the company pane already uses.
 */
import { useCallback, useEffect, useRef, useState } from 'react'

import { useChiefApi } from '@/providers/ApiSessionProvider'
import { ChiefApiError, type ChiefApiErrorShape } from '@/types/ApiErrors'
import { type ConversationRow, rowsFromTranscript } from '@/types/Conversation'
import type { FounderConversationResult, FounderLaunched } from '@/types/Founder'
import { isNullish } from '@/utils/Nullish'

function errorFrom(error: unknown): ChiefApiErrorShape {
  if (error instanceof ChiefApiError) {
    return { kind: error.kind, status: error.status, code: error.code, detail: error.detail }
  }
  return { kind: 'network', detail: error instanceof Error ? error.message : 'Unknown failure' }
}

export function useFounderConversation(): FounderConversationResult {
  const api = useChiefApi()
  const [rows, setRows] = useState<readonly ConversationRow[]>([])
  const [launched, setLaunched] = useState<FounderLaunched | undefined>(undefined)
  const [pending, setPending] = useState(false)
  const [hydrating, setHydrating] = useState(true)
  const [error, setError] = useState<ChiefApiErrorShape | undefined>(undefined)
  const sending = useRef(false)

  const read = useCallback(
    async (signal?: AbortSignal): Promise<void> => {
      const transcript = await api.founderTranscript(signal)
      if (signal?.aborted === true) return
      setRows(rowsFromTranscript(transcript.entries))
      // Only SET, never clear. A launch is durable for the life of the
      // conversation, and this read runs immediately after `say` has already
      // reported one — so clearing on an absent field made the turn's own
      // answer disappear a tick after it arrived, and the "Open <company>"
      // link with it. Caught by this hook's test, which is what a re-read
      // racing a reply looks like from the operator's side.
      //
      // The `isNullish` guard is also the wire's own reading: `launched` is
      // `nullish`, so an absent launch and a null one are one answer, and
      // `null` in state would render a link to `/c/undefined`.
      if (!isNullish(transcript.launched)) setLaunched(transcript.launched)
    },
    [api]
  )

  useEffect(() => {
    const controller = new AbortController()
    setHydrating(true)
    void read(controller.signal)
      .catch((failure: unknown) => {
        if (controller.signal.aborted) return
        setError(errorFrom(failure))
      })
      .finally(() => {
        if (controller.signal.aborted) return
        setHydrating(false)
      })
    return () => controller.abort()
  }, [read])

  const send = useCallback(
    async (text: string): Promise<void> => {
      // A second turn cannot start while one is running: `AgentHarness.prompt`
      // is not re-entrant, and the honest UI for that is a composer that waits
      // rather than a queue this agent does not have.
      if (sending.current) return
      sending.current = true
      setPending(true)
      setError(undefined)
      try {
        const outcome = await api.founderSay(text)
        if (!isNullish(outcome.launched)) setLaunched(outcome.launched)
      } catch (failure) {
        setError(errorFrom(failure))
      } finally {
        sending.current = false
        setPending(false)
      }
      // Read the transcript whether the turn succeeded or failed. A refused
      // turn still put the operator's own message in the session, and a page
      // that dropped it would show them typing into nothing.
      try {
        await read()
      } catch (failure) {
        setError(errorFrom(failure))
      }
    },
    [api, read]
  )

  const abort = useCallback(async (): Promise<void> => {
    try {
      await api.founderAbort()
    } catch (failure) {
      setError(errorFrom(failure))
    }
  }, [api])

  return { rows, pending, hydrating, error, launched, send, abort }
}

/** Public types for the person event stream.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`.
 *
 * These are Pi's own event NAMES with this program's own sanitised payloads.
 * The names are Pi's because the browser's fold (`foldSessionEvent`) reads
 * them and a private vocabulary here meant a live agent's pane stayed empty;
 * the payloads are ours because the harness's real ones carry provider
 * requests, whole conversations and arbitrary tool results. */

/** One event, as a browser may receive it. */
export type PersonStreamFrame =
  | { readonly type: 'agent_start' | 'turn_start' | 'turn_end' | 'agent_end' }
  | {
      readonly type: 'message_start' | 'message_update' | 'message_end'
      /** The assistant message reduced to its readable text parts. */
      readonly message: Record<string, unknown>
    }
  | {
      readonly type: 'tool_execution_start'
      readonly toolName: string
      readonly toolCallId: string
    }
  | {
      readonly type: 'tool_execution_end'
      readonly toolName: string
      readonly toolCallId: string
      readonly isError: boolean
    }

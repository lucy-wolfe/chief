/**
 * Ephemeral, in-process activity status line for the Pi footer (task #27,
 * the operator's parallel P0 "FIVE").
 *
 * This module owns the real-time answer to "what is this agent actually doing
 * right now" as a single short label, driven purely by Pi extension events:
 *
 *   - working               — a tool is executing, or the model is streaming
 *                             a turn
 *   - (nothing)             — the agent is idle
 *
 * Design contract (governed by DECISIONS.md 2026-07-20 "FIVE" and
 * the design record):
 *   - Empty is not broken: an idle agent renders NOTHING, never a placeholder.
 *   - Truthful-or-absent: every label is derived from an event this process
 *     actually observed. There is no read here at all — the whole state is
 *     ephemeral and in-process — so there is nothing to go stale and no
 *     plausible default to fabricate.
 *   - Read-only: this module writes no org state and touches no file. It only
 *     hands a string (or undefined) to the injected `setStatus` sink, which the
 *     footer surfaces through Pi's `ctx.ui.setStatus` extension-status channel.
 *
 * The durable counts the operator also wants visible are already disk-authoritative
 * footer fields (team-ui's reminder and mailbox counts, read off-render through
 * the bounded projections); this module is the live-verb layer that overlays
 * them, and it never re-reads that authority on the hot path.
 *
 * Self-contained: this file is copied verbatim into every person's pi-home, so
 * it must not import launcher source (`../src/...`) — enforced by the
 * org-materialize drift check.
 */

/** The extension-status key this line owns in the footer's status map. */
export const ACTIVITY_STATUS_KEY = "activity";

export const WORKING_LABEL = "⚙ working";

export interface ActivityStatusDeps {
  /**
   * Sink for the rendered label; called only when the visible text actually
   * changes. `undefined` clears the status (renders nothing).
   */
  setStatus: (text: string | undefined) => void;
}

export interface ActivityStatusLine {
  /** A tool began executing. */
  toolStart(toolCallId: string): void;
  /** A tool finished. */
  toolEnd(toolCallId: string): void;
  /** The model began streaming a turn (agent_start). */
  streamingStarted(): void;
  /** The run fully settled (agent_settled) — no tool, no stream. */
  streamingSettled(): void;
  /** Session teardown: cancel the flash timer and clear the status. */
  reset(): void;
  /** The label currently shown, or undefined when nothing renders. */
  current(): string | undefined;
}

/**
 * Build the ephemeral activity status line. All state is in-process; nothing is
 * read or written. `setStatus` fires only on a genuine change so the footer is
 * not asked to redraw for a no-op.
 */
export function createActivityStatusLine(deps: ActivityStatusDeps): ActivityStatusLine {
  // Concurrent tools are tracked by call id so overlapping executions resolve
  // correctly rather than whichever ended last.
  const running = new Set<string>();
  let streaming = false;
  let lastEmitted: string | undefined;

  // A live tool or stream is the truth; an idle agent renders nothing.
  const render = (): string | undefined => (running.size || streaming ? WORKING_LABEL : undefined);

  const emit = (): void => {
    const next = render();
    if (next === lastEmitted) return;
    lastEmitted = next;
    deps.setStatus(next);
  };

  return {
    toolStart(toolCallId) {
      running.add(toolCallId);
      emit();
    },
    toolEnd(toolCallId) {
      running.delete(toolCallId);
      emit();
    },
    streamingStarted() {
      streaming = true;
      emit();
    },
    streamingSettled() {
      streaming = false;
      emit();
    },
    reset() {
      running.clear();
      streaming = false;
      emit();
    },
    current() {
      return render();
    },
  };
}

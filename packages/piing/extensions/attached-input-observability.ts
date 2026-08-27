/**
 * Privacy-safe evidence for an interactive Pi submission. Runtime does not expose
 * an attached client's keystrokes to an application, so `pi_input` is the
 * first boundary chiefd can authoritatively observe. We deliberately
 * retain no text, text hash, credentials, or terminal escape data.
 */
export type AttachedInputBoundary = "attach_handoff" | "pi_input" | "transcript_user_persisted" | "turn_started";

export interface AttachedInputTrace {
  id: string;
  sessionId: string | undefined;
  paneId: string | undefined;
  lastBoundary: AttachedInputBoundary;
}

export interface AttachedInputTraceLogger {
  (trace: AttachedInputTrace): void;
}

export function opaqueAttachedInputId(next: () => string = () => crypto.randomUUID()): string {
  return `attach-input-${next()}`;
}

/** Count only Pi's persisted user entries; their content never leaves Pi. */
export function persistedUserEntryCount(entries: readonly unknown[]): number {
  return entries.filter((entry) => {
    if (!entry || typeof entry !== "object") return false;
    const value = entry as { type?: unknown; message?: { role?: unknown } };
    return value.type === "message" && value.message?.role === "user";
  }).length;
}

/**
 * Tracks one submitted input without retaining it. The caller supplies the
 * session snapshot because only Pi owns transcript persistence ordering.
 */
export class AttachedInputTracer {
  private trace: AttachedInputTrace | undefined;
  private userEntriesBeforeInput = 0;

  constructor(private readonly log: AttachedInputTraceLogger, private readonly nextId = opaqueAttachedInputId) {}

  inputReceived(sessionId: string | undefined, paneId: string | undefined, entries: readonly unknown[]): AttachedInputTrace {
    this.userEntriesBeforeInput = persistedUserEntryCount(entries);
    this.trace = { id: this.nextId(), sessionId, paneId, lastBoundary: "pi_input" };
    this.log(this.trace);
    return this.trace;
  }

  transcriptChecked(entries: readonly unknown[]): AttachedInputTrace | undefined {
    if (!this.trace || this.trace.lastBoundary !== "pi_input") return this.trace;
    if (persistedUserEntryCount(entries) <= this.userEntriesBeforeInput) return this.trace;
    this.trace = { ...this.trace, lastBoundary: "transcript_user_persisted" };
    this.log(this.trace);
    return this.trace;
  }

  turnStarted(): AttachedInputTrace | undefined {
    // A turn that arrives before Pi's transcript has acknowledged this input
    // could belong to an extension wake or an older queued submission. Never
    // attribute it to the interactive input in that ambiguous state.
    if (!this.trace || this.trace.lastBoundary !== "transcript_user_persisted") return this.trace;
    this.trace = { ...this.trace, lastBoundary: "turn_started" };
    this.log(this.trace);
    return this.trace;
  }

  diagnostic(): string | undefined {
    if (!this.trace) return undefined;
    return `ChiefD attached-input trace ${this.trace.id}: last observed boundary=${this.trace.lastBoundary}; session=${this.trace.sessionId ?? "unknown"}; pane=${this.trace.paneId ?? "unknown"}. No message content was recorded.`;
  }
}

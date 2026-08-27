/**
 * The replay identity of an `org_send`, kept in its own module.
 *
 * It lives beside `organization-intercom.ts` rather than inside it because
 * this is a decision, not a transport: it is provable on its own, and the
 * intercom's export surface is under a shrinking quarantine that a new
 * top-level export would work against.
 */

/**
 * How long an identical send is treated as a REPLAY of the interrupted call
 * rather than a deliberate repeat.
 *
 * A kill-and-resume replay is bounded by relaunch + session restore + the
 * model re-issuing the call; it is not bounded by anything the sender chooses.
 * Ten minutes clears that round trip with room to spare while staying well
 * inside the interval at which a person deliberately re-asks the same question
 * in the same words. The window only ever decides which of two behaviours a
 * duplicate gets, and BOTH are reported: inside it the prior delivery is named
 * back to the caller, outside it a second message is delivered.
 */
export const ORGANIZATION_SEND_REPLAY_WINDOW_MS = 600_000;

/** Bounded so a corrupt mailbox cannot turn a send into an unbounded scan. */
const ORGANIZATION_SEND_REPLAY_SCAN_LIMIT = 64;

/**
 * The candidate ids for one fingerprint, oldest first: `msg-<fp>`,
 * `msg-<fp>-2`, `msg-<fp>-3`, ...
 *
 * A deliberate repeat outside the window takes the next index, which keeps the
 * scheme deterministic under a replay OF THAT REPEAT: the replay sees the same
 * prior messages and therefore computes the same index.
 */
function messageReplayCandidateId(fingerprint: string, index: number): string {
  return index === 0 ? `msg-${fingerprint}` : `msg-${fingerprint}-${index + 1}`;
}

/** The minimum a prior delivery has to tell us to be recognized as one. */
export type PriorDelivery = { createdAt: string };

/**
 * Decide which id this attempt owns, and whether it is the replay of one
 * already delivered.
 *
 * Scans the fingerprint's candidate ids oldest-first. The first FREE index is
 * a new send. An OCCUPIED index inside the window means this attempt is that
 * message's replay; occupied and outside it means the sender is deliberately
 * repeating itself and takes the next index.
 *
 * The lookup is injected so this decision — the whole of the new behaviour —
 * is provable without a mailbox, a daemon, or a clock.
 */
export async function resolveSendReplay(input: {
  fingerprint: string;
  nowMs: number;
  windowMs?: number;
  lookup: (candidateId: string) => Promise<PriorDelivery | undefined>;
}): Promise<{ id: string; replayedFrom?: string }> {
  const windowMs = input.windowMs ?? ORGANIZATION_SEND_REPLAY_WINDOW_MS;
  for (let index = 0; index < ORGANIZATION_SEND_REPLAY_SCAN_LIMIT; index += 1) {
    const candidate = messageReplayCandidateId(input.fingerprint, index);
    const prior = await input.lookup(candidate);
    if (!prior) return { id: candidate };
    const priorMs = Date.parse(prior.createdAt);
    // An unparseable stored stamp cannot prove the prior is recent. Treat it
    // as settled and move to the next index: delivering twice is recoverable,
    // swallowing a message on the strength of a corrupt timestamp is not.
    if (Number.isFinite(priorMs) && Number.isFinite(input.nowMs)
      && input.nowMs - priorMs <= windowMs) {
      return { id: candidate, replayedFrom: prior.createdAt };
    }
  }
  // A fingerprint whose whole candidate range is occupied is a caller sending
  // the same words 64 times. Deliver rather than refuse: a lost message is the
  // failure this change exists to prevent.
  return { id: messageReplayCandidateId(input.fingerprint, ORGANIZATION_SEND_REPLAY_SCAN_LIMIT) };
}

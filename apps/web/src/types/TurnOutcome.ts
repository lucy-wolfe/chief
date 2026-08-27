/** Public types for turn outcomes.
 *
 * Housed here per `lucy/no-exported-type-outside-types-dir`. */

/** Which kind of failure a turn's error text describes.
 *
 * Four cases because an operator acts differently on each: a refused
 * credential is durable and no amount of retrying changes it, a rate limit is
 * the one genuinely retryable case, a rejected request is this product's own
 * defect, and everything else is transport. Collapsing them once cost real
 * time — a rejected key surfaced as `"Connection error."` and sent a reader to
 * check a network that was answering fine. */
export type TurnFailure = 'credential-rejected' | 'rate-limited' | 'rejected-request' | 'transport'

// Types for RemindersClient — the durable-reminder surface (`/v1/reminders/*`).
//
// Source: src/organization/org-reminder-store.ts:58-104, re-verified against
// apps/chiefd/crates/chiefd-core/src/store/supervision.rs `Reminder`.

/** Rust authority: chiefd-core/src/store/supervision.rs `Reminder`. `status`
 * is a plain `String` on the wire (`"active" | "stopped"`) — kept as a widened
 * union with a trailing `string` arm since Rust does not close it to an enum.
 * `fireCount` is always serialized server-side (`#[serde(default)]`, no
 * `skip_serializing_if`) but kept optional here as defensive parsing, matching
 * the existing org-reminder-store.ts contract. */
export interface Reminder {
  id: string
  personId: string
  createdByPersonId: string
  prompt: string
  intervalMs: number
  /** ISO-8601 instant of the next fire. */
  nextDueAt: string
  status: 'active' | 'stopped' | string
  recurring: boolean
  fireCount?: number
  createdAt: string
  /** Absent until it first fires — distinguishes "never evaluated" from
   * "evaluated and re-armed". */
  lastFiredAt?: string
  expiresAt?: string
}

/** No `createdByPersonId`: who armed a reminder is the same fact as who was
 * allowed to arm it, and since #751/P7 that is the enrolled key the caller
 * presents. chiefd fills `Reminder.createdByPersonId` from the verified
 * credential and refuses a caller who neither is, nor manages, `personId`. */
export interface ArmReminderInput {
  slug: string
  personId: string
  prompt: string
  intervalMs: number
  /** Defaults to `true` server-side. */
  recurring?: boolean
  expiresAt?: string
}

export interface ListRemindersInput {
  slug: string
  personId: string
}

export interface StopReminderInput {
  slug: string
  personId: string
  reminderId: string
}

export interface ReminderResult {
  reminder: Reminder
}

export interface ListRemindersResult {
  reminders: Reminder[]
}

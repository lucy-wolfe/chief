import { ChiefdUnavailableError, ReminderRefusalError } from '@/Errors'
import { decodeRefusal, isRefusalStatus } from '@/resources/OrgRoutes'
import type {
  ArmReminderInput,
  ListRemindersInput,
  ListRemindersResult,
  ReminderResult,
  StopReminderInput
} from '@/types/Reminders'
import type { HttpTransport } from '@/types/Transport'

/** Every method sends the caller's `slug` verbatim: it is already the company
 * key (`sha256(dir)[..12]`, read off the beacond row or the daemon rendezvous).
 * `companyKeyed`, which rewrote it into the composite `documentKey(slug,
 * root)`, is deleted with the composite.
 *
 * The reminder routes answer the same `{code, detail}` body as every other
 * chiefd route now that `company_error` projects through the one status table,
 * so this client reads the SHARED refusal set instead of the `{400, 404}` it
 * hard-coded — a 409 or a 422 from these routes used to be reported as an
 * outage that had not happened, with chiefd's reason discarded. Anything
 * outside the refusal set stays a genuine infra failure. */
export class RemindersClient {
  constructor(protected readonly transport: HttpTransport) {}

  /** `JSON.parse` returns `any`, so `return JSON.parse(...)` from a function
   * declared `Promise<R>` needs no assertion — the reminder routes were
   * never shape-validated beyond the refusal check in the legacy client
   * either (org-reminder-store.ts's own `post<R>`). */
  private async post<R>(path: string, body: unknown): Promise<R> {
    const response = await this.transport.post(path, body)
    if (response.status < 200 || response.status >= 300) {
      if (isRefusalStatus(response.status)) {
        const { code, detail } = decodeRefusal(response.body)
        throw new ReminderRefusalError({ status: response.status, code, detail })
      }
      throw new ChiefdUnavailableError({
        kind: 'http-error',
        url: '',
        path,
        status: response.status,
        detail: decodeRefusal(response.body).detail
      })
    }
    try {
      return JSON.parse(response.body)
    } catch (error) {
      throw new ChiefdUnavailableError({ kind: 'malformed-body', url: '', path, cause: error })
    }
  }

  async armReminder(input: ArmReminderInput): Promise<ReminderResult> {
    return this.post<ReminderResult>('/v1/reminders/arm', input)
  }

  async listReminders(input: ListRemindersInput): Promise<ListRemindersResult> {
    return this.post<ListRemindersResult>('/v1/reminders/list', input)
  }

  async stopReminder(input: StopReminderInput): Promise<ReminderResult> {
    return this.post<ReminderResult>('/v1/reminders/stop', input)
  }
}

/** Client-side courtesy constant. */
export const MIN_REMINDER_INTERVAL_MS = 60_000

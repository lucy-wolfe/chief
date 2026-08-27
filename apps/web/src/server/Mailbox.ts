/**
 * One person's mail, as a browser can read it.
 *
 * # Why this is not a pass-through
 *
 * chiefd answers `/v1/org/mailbox/read-person` with a row read whose
 * `document` is a SERIALIZED JSON string — `{"entries":[…]}` — because the
 * docstore stores documents as text and hands them back as text. The route
 * used to forward that verbatim while the browser's schema declared
 * `{pendingCount, envelopes}`, a shape apps/api synthesised and nothing sends
 * any more, so every mailbox read threw a ZodError in the client.
 *
 * The seam is closed HERE rather than in the browser on purpose: a second JSON
 * parse in the client would make the page a reader of chiefd's storage format,
 * and `pending` would then be counted in two places. It is counted once, on
 * the server, against chiefd's own vocabulary.
 *
 * # `pending` is the only live bucket
 *
 * chiefd's `MailboxState` has one live bucket and five terminal ones
 * (`delivered`, `accepted`, `superseded`, `rejected`, `resolved`). Counting
 * anything but `pending` would put archived mail on the badge an operator uses
 * to decide whether somebody needs waking. An entry whose state is unreadable
 * is NOT counted as pending — chiefd's own parser refuses to default it, for
 * the same reason.
 */
import { companyChiefd } from '@/server/CompanyChiefd'
import type { MailboxRead } from '@/types/Mailbox'
import { isNullish } from '@/utils/Nullish'

/** chiefd's live bucket. Every other bucket is an archive state. */
const PENDING = 'pending'

function entriesOf(document: string | undefined): unknown[] {
  if (isNullish(document)) return []
  let parsed: unknown
  try {
    parsed = JSON.parse(document)
  } catch {
    // A document this server cannot parse is reported as an EMPTY mailbox
    // rather than as a failure: the mailbox is a badge beside a person, and
    // failing the whole pane over an unreadable row would hide a working
    // agent behind a storage detail.
    return []
  }
  if (typeof parsed !== 'object' || isNullish(parsed) || Array.isArray(parsed)) return []
  const { entries } = Object.fromEntries(Object.entries(parsed))
  return Array.isArray(entries) ? entries : []
}

function isPending(entry: unknown): boolean {
  if (typeof entry !== 'object' || isNullish(entry)) return false
  const { state } = Object.fromEntries(Object.entries(entry))
  return state === PENDING
}

/** This person's mailbox: how much is waiting, and the entries themselves. */
export async function personMailbox(companyKey: string, personId: string): Promise<MailboxRead> {
  const chiefd = await companyChiefd(companyKey)
  const row = await chiefd.mailbox.readPerson(companyKey, personId)
  const entries = entriesOf(row.document)
  return {
    personId,
    pendingCount: entries.filter(isPending).length,
    // Forwarded opaque. An envelope carries a body, an assignment and a
    // health incident, and this layer has no business reshaping any of it —
    // it counts what is live and passes the rest through.
    envelopes: entries
  }
}

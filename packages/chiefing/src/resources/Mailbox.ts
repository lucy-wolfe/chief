import { normalizeRowRead, postOrgRoute, withReadOpts } from '@/resources/OrgRoutes'
import type { ReadOpts, RowReadResult, WireRowRead } from '@/types/OrgDocs'
import type { HttpTransport } from '@/types/Transport'

/** Rust authority: apps/chiefd/crates/chiefd-api/src/docstore/router.rs
 * `org_mailbox_read`/`org_mailbox_read_person`/`org_mailbox_delta`/
 * `org_mailbox_list_persons`. Company-keyed via `root`.
 *
 * The whole-mailbox `publish` is deleted with its route: the publisher-route
 * sweep found no caller, and the mailbox is written by the send/delivery
 * verbs and the O(delta) path below, never by handing back a whole
 * document. */
export class MailboxClient {
  constructor(
    protected readonly transport: HttpTransport,
    protected readonly url: string = ''
  ) {}

  async read(slug: string, opts?: ReadOpts): Promise<RowReadResult> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      '/v1/org/mailbox/read',
      withReadOpts({ slug }, opts)
    )
    return normalizeRowRead(wire)
  }

  async readPerson(slug: string, personId: string, opts?: ReadOpts): Promise<RowReadResult> {
    const wire = await postOrgRoute<WireRowRead>(
      this.transport,
      this.url,
      '/v1/org/mailbox/read-person',
      withReadOpts({ slug, personId }, opts)
    )
    return normalizeRowRead(wire)
  }

  /** The O(delta) fence-free path — always applies (no CAS). */
  async delta(
    slug: string,
    personId: string,
    upserts: string,
    deletes: string[],
    at: string
  ): Promise<void> {
    await postOrgRoute(this.transport, this.url, '/v1/org/mailbox/delta', {
      slug,
      personId,
      upserts,
      deletes,
      at
    })
  }

  async listPersons(slug: string): Promise<string[]> {
    const wire = await postOrgRoute<{ personIds: string[] }>(
      this.transport,
      this.url,
      '/v1/org/mailbox/list-persons',
      { slug }
    )
    return wire.personIds
  }
}

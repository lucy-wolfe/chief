import { AggregatesClient } from '@/resources/Aggregates'
import { ApiHostLaunchProfileClient } from '@/resources/ApiHostLaunchProfile'
import { AuthClient } from '@/resources/Auth'
import { DocsClient } from '@/resources/Docs'
import { MailboxClient } from '@/resources/Mailbox'
import { ManifestClient } from '@/resources/Manifest'
import { OrgSliceClient } from '@/resources/OrgSlice'
import { PersonContractsClient } from '@/resources/PersonContracts'
import { RemindersClient } from '@/resources/Reminders'
import { RowStoresClient } from '@/resources/RowStores'
import { RuntimeClient } from '@/resources/Runtime'
import { SessionLifecycleClient } from '@/resources/SessionLifecycle'
import { SettingsClient } from '@/resources/Settings'
import { StaffingClient } from '@/resources/Staffing'
import { subscribeSse } from '@/sse/SseHub'
import { FetchTransport } from '@/transport/FetchTransport'
import type { ChiefdClientOptions } from '@/types/Transport'
import type { SseSubscription, WatchSubscribeOptions } from '@/types/Watch'

export class ChiefdClient {
  readonly url: string
  readonly docs: DocsClient
  readonly auth: AuthClient
  readonly manifest: ManifestClient
  readonly aggregates: AggregatesClient
  readonly apiHostLaunchProfile: ApiHostLaunchProfileClient
  readonly mailbox: MailboxClient
  readonly orgSlice: OrgSliceClient
  readonly personContracts: PersonContractsClient
  readonly rows: RowStoresClient
  /** The supervision & session-lifecycle verbs (chiefd decides, TS asks). */
  readonly sessionLifecycle: SessionLifecycleClient
  /** Runtime lifecycle, runtime placement, materialization, model/thinking
   * commands and the company-session-action verbs — the Rust replacement for
   * the deleted `apps/cli/src/legacy/organization` runtime cluster. */
  readonly runtime: RuntimeClient
  readonly staffing: StaffingClient
  readonly reminders: RemindersClient
  readonly settings: SettingsClient

  constructor(options: ChiefdClientOptions) {
    // #929/#936: the row-store factory this client's `rows` surface replaced
    // (`orgRowStoreForTribe`/`orgRowStoreAmbient`, apps/cli's
    // org-row-stores.ts) carried a synchronous empty-URL fail-closed guard
    // -- `#929` moved three consumers onto this client without an
    // equivalent, a disclosed gap rather than a silent one. An empty/
    // whitespace-only URL here would otherwise construct a `FetchTransport`
    // that fails per-request with an opaque network error far from this
    // call site, instead of refusing immediately with a clear cause.
    if (!options.url.trim()) {
      throw new Error('ChiefdClient requires a non-empty chiefd URL')
    }
    this.url = options.url
    const transport =
      options.transport ??
      // No timeout is passed, and none may be: `FetchTransport`'s
      // `DEFAULT_TIMEOUT_MS` is the one definition of the client's patience,
      // and it is the number `scripts/test/client-observable-wait.test.mjs`
      // holds against chiefd's own bounds. This class used to carry a second
      // `defaultTimeoutMs = 10_000` of its own, so every `ChiefdClient` caller
      // — including `apps/web` — kept the abandoned patience no matter what
      // the transport's constant said, and the guard read a number nobody used.
      new FetchTransport(options.url, undefined, options.authHeaderProvider, options.authInvalidate)
    this.docs = new DocsClient(transport)
    this.auth = new AuthClient(transport)
    this.manifest = new ManifestClient(transport, options.url)
    this.aggregates = new AggregatesClient(transport, options.url)
    this.apiHostLaunchProfile = new ApiHostLaunchProfileClient(transport, options.url)
    this.mailbox = new MailboxClient(transport, options.url)
    this.orgSlice = new OrgSliceClient(transport, options.url)
    this.personContracts = new PersonContractsClient(transport, options.url)
    this.rows = new RowStoresClient(transport, options.url)
    this.sessionLifecycle = new SessionLifecycleClient(transport, options.url)
    this.runtime = new RuntimeClient(transport, options.url)
    this.staffing = new StaffingClient(transport)
    this.reminders = new RemindersClient(transport)
    this.settings = new SettingsClient(transport, options.url)
  }

  /** Hub-multiplexed /v1/docs/watch subscription bound to this client's URL.
   * `options.slug` travels verbatim: it is already the company key
   * (`sha256(dir)[..12]`), like every other route this client posts. The
   * root-aware translation that used to happen here is deleted with the
   * composite key it built. */
  watch(options: WatchSubscribeOptions): SseSubscription {
    return subscribeSse({ ...options, url: this.url })
  }
}

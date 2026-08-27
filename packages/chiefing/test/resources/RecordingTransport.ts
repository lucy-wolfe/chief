// A recording fake `HttpTransport` shared by this repo's resource tests
// (originally #773's Manifest/Aggregates/Memory/Mailbox/PersonContracts/
// RowStores suites; #774's Staffing/Tasks/Reminders suites reuse it rather
// than duplicating a second recorder — "ONE implementation"). Not a
// `.test.ts` file itself — a plain helper module `enforce-test-file-location`
// does not govern.

import type { HttpResponse, HttpTransport } from '@/types/Transport'

interface RecordedCall {
  method: 'GET' | 'POST'
  path: string
  body?: unknown
}

type Responder = (call: RecordedCall) => HttpResponse

/** Records every call and answers each with a caller-supplied responder —
 * the "recording fake `HttpTransport`" the story's Contract asks for. */
export class RecordingTransport implements HttpTransport {
  readonly calls: RecordedCall[] = []

  constructor(private readonly responder: Responder) {}

  async post(path: string, body: unknown): Promise<HttpResponse> {
    const call: RecordedCall = { method: 'POST', path, body }
    this.calls.push(call)
    return this.responder(call)
  }

  async get(path: string): Promise<HttpResponse> {
    const call: RecordedCall = { method: 'GET', path }
    this.calls.push(call)
    return this.responder(call)
  }
}

/** Build a fixed-response transport: every call gets the same status/body,
 * regardless of path. */
export function fixedResponseTransport(status: number, body: unknown): RecordingTransport {
  return new RecordingTransport(() => jsonResponse(status, body))
}

export function jsonResponse(status: number, body: unknown): HttpResponse {
  /* eslint-disable lucy/no-json-stringify */
  // Test-only fixture encoder — @tribes-terminal/foundation is not a
  // dependency anywhere in this workspace (see FetchTransportTest.test.ts's
  // matching disable block).
  const encoded = JSON.stringify(body)
  /* eslint-enable lucy/no-json-stringify */
  return { status, body: encoded }
}

/** Plain-text fixture body — never JSON-encoded, unlike `jsonResponse`.
 * `RemindersClient`'s refusal decode reads a raw-text 400/404 body (E2-S5,
 * #774), so this is the one place a test needs a non-JSON response. */
export function textResponse(status: number, body: string): HttpResponse {
  return { status, body }
}

// The Founder's talk verbs.
//
// The failure paths are the substance. A turn the provider did not complete
// comes back as a perfectly ordinary `AssistantMessage` with `stopReason:
// 'error'` and no content, and returning that as a 200 with an empty reply is
// how an agent comes to look quiet instead of broken.
import { describe, expect, it, vi } from 'vitest'

import { FounderUnavailableError } from '@/server/FounderAgent'

const mocks = vi.hoisted(() => ({
  founderAgent: vi.fn(),
  hostedFounder: vi.fn()
}))

vi.mock('@/server/FounderAgent', async () => {
  const actual =
    await vi.importActual<typeof import('@/server/FounderAgent')>('@/server/FounderAgent')
  return {
    ...actual,
    founderAgent: mocks.founderAgent,
    hostedFounder: mocks.hostedFounder
  }
})

import { abort, say, transcript } from '@/server/FounderTalk'

interface FakeMessage {
  role: 'assistant'
  content: unknown
  stopReason?: string
  errorMessage?: string
}

/** A hosted Founder whose harness answers with `message`. */
function hosted(message: FakeMessage, launched?: { slug: string; name: string }): unknown {
  return {
    harness: {
      prompt: vi.fn(async () => message),
      abort: vi.fn(async () => ({ clearedSteer: [], clearedFollowUp: [] }))
    },
    session: {
      getBranch: async () => [
        { type: 'message', id: 'e1', message: { role: 'user', content: 'hello' } },
        {
          type: 'message',
          id: 'e2',
          message: { role: 'assistant', content: [{ type: 'text', text: 'hi' }] }
        },
        // A tool result is real transcript content and is NOT conversation.
        { type: 'message', id: 'e3', message: { role: 'toolResult', content: [] } }
      ]
    },
    route: { provider: 'zip', model: 'fast-1' },
    launch: typeof launched === 'undefined' ? {} : { launched }
  }
}

function reply(text: string): FakeMessage {
  return { role: 'assistant', content: [{ type: 'text', text }] }
}

function failed(errorMessage: string): FakeMessage {
  return { role: 'assistant', content: [], stopReason: 'error', errorMessage }
}

describe('FounderTalk.say', () => {
  it('refuses an empty turn without starting a Founder', async () => {
    mocks.founderAgent.mockReset()
    await expect(say('   ')).rejects.toThrowError(/a turn needs text/)
    expect(mocks.founderAgent).not.toHaveBeenCalled()
  })

  it('answers with the assistant’s readable text', async () => {
    mocks.founderAgent.mockResolvedValue(hosted(reply('What is it called?')))
    expect(await say('I want a company')).toEqual({ reply: 'What is it called?' })
  })

  it('reports the company THIS turn created', async () => {
    /* eslint-disable @typescript-eslint/consistent-type-assertions */
    // The fake is deliberately `unknown` so it cannot drift into claiming to
    // be a real `HostedFounder`; this one narrowing is what lets the test
    // replace its prompt.
    const founder = hosted(reply('Done.')) as {
      harness: { prompt: unknown }
      launch: { launched?: unknown }
    }
    /* eslint-enable @typescript-eslint/consistent-type-assertions */
    // The tool runs INSIDE the turn, so the record appears between `say`
    // reading it and the answer arriving. That ordering is the whole reason
    // the launch travels as a mutable record rather than a return value, and a
    // fake that set it before the prompt would prove nothing about it.
    founder.harness.prompt = async (): Promise<FakeMessage> => {
      founder.launch.launched = { slug: 'acme-inc', name: 'Acme Inc' }
      return reply('Done.')
    }
    mocks.founderAgent.mockResolvedValue(founder)
    expect(await say('call it Acme Inc')).toEqual({
      reply: 'Done.',
      launched: { slug: 'acme-inc', name: 'Acme Inc' }
    })
  })

  it('does not re-announce a company an earlier turn created', async () => {
    // A conversation that keeps reporting the same launch would make the page
    // re-render its "Open Acme Inc" banner as if a second company had appeared.
    mocks.founderAgent.mockResolvedValue(
      hosted(reply('Anything else?'), {
        slug: 'acme-inc',
        name: 'Acme Inc'
      })
    )
    expect(await say('thanks')).toEqual({ reply: 'Anything else?' })
  })

  it('names a rejected credential as a credential, not a connection error', async () => {
    // The exact misreport this taxonomy exists for: a 401 once surfaced as
    // "Connection error." and sent a reader to check a working network.
    mocks.founderAgent.mockResolvedValue(hosted(failed('401 Unauthorized')))
    await expect(say('hello')).rejects.toMatchObject({
      status: 409,
      code: 'provider-credential-rejected'
    })
  })

  it('names a rate limit as retryable', async () => {
    mocks.founderAgent.mockResolvedValue(hosted(failed('429 rate limit exceeded')))
    await expect(say('hello')).rejects.toMatchObject({
      status: 429,
      code: 'provider-rate-limited'
    })
  })

  it('names a rejected request as this product’s defect', async () => {
    mocks.founderAgent.mockResolvedValue(hosted(failed('400 invalid schema for tool')))
    await expect(say('hello')).rejects.toMatchObject({
      status: 502,
      code: 'provider-rejected-request'
    })
  })

  it('falls through an unrecognised failure to transport rather than asserting a cause', async () => {
    mocks.founderAgent.mockResolvedValue(hosted(failed('Connection error.')))
    const failure = await say('hello').catch((error: unknown) => error)
    expect(failure).toBeInstanceOf(FounderUnavailableError)
    expect(failure).toMatchObject({ status: 502, code: 'turn-failed' })
  })

  it('never reports a failed turn as an empty reply', async () => {
    mocks.founderAgent.mockResolvedValue(hosted(failed('Connection error.')))
    await expect(say('hello')).rejects.toThrowError()
  })
})

describe('FounderTalk.transcript', () => {
  it('is empty for a Founder nobody has started', async () => {
    // The page's FIRST request. Refusing here would greet every visitor with
    // an error banner before they had asked for anything.
    mocks.hostedFounder.mockReturnValue(undefined)
    expect(await transcript()).toEqual({ entries: [] })
  })

  it('returns only the readable conversation, in the shape the browser folds', async () => {
    mocks.hostedFounder.mockReturnValue(hosted(reply('hi'), { slug: 'acme', name: 'Acme' }))
    const result = await transcript()
    expect(result.entries).toEqual([
      {
        type: 'message',
        id: 'e1',
        message: { role: 'user', content: [{ type: 'text', text: 'hello' }] }
      },
      {
        type: 'message',
        id: 'e2',
        message: { role: 'assistant', content: [{ type: 'text', text: 'hi' }] }
      }
    ])
    expect(result.launched).toEqual({ slug: 'acme', name: 'Acme' })
  })
})

describe('FounderTalk.abort', () => {
  it('reports nothing to stop rather than starting a Founder to stop it', async () => {
    mocks.hostedFounder.mockReturnValue(undefined)
    mocks.founderAgent.mockReset()
    expect(await abort()).toEqual({ aborted: false })
    expect(mocks.founderAgent).not.toHaveBeenCalled()
  })

  it('stops the turn a live Founder is running', async () => {
    mocks.hostedFounder.mockReturnValue(hosted(reply('hi')))
    expect(await abort()).toEqual({ aborted: true })
  })
})

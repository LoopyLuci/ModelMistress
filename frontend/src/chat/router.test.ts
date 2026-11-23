import { afterEach, describe, expect, it, vi } from 'vitest'
import { setToken } from '@/transport'
import { AUTO, pickFor, pickText, routeLine, routerStatus, setRole, suitsRole, withoutRoleTags, withoutTagsInLastUserMessage } from './router'

const reply = (result: unknown) =>
  new Response(JSON.stringify({ result }), { status: 200, headers: { 'Content-Type': 'application/json' } })

/** A hub that answers every call with `result`; returns the mock so a test can read what was sent. */
function hub(result: unknown = {}) {
  const fetchMock = vi.fn(async () => reply(result))
  vi.stubGlobal('fetch', fetchMock)
  setToken('secret')
  return fetchMock
}
const sent = (m: ReturnType<typeof hub>, n = 0) => {
  const [url, init] = m.mock.calls[n] as unknown as [string, RequestInit]
  return { url, body: JSON.parse(String(init.body)) }
}

afterEach(() => {
  vi.unstubAllGlobals()
  setToken('')
})

describe('withoutRoleTags', () => {
  it('removes a role tag and the space after it', () => {
    expect(withoutRoleTags('@deep prove this')).toBe('prove this')
    expect(withoutRoleTags('hello @main world')).toBe('hello world')
    expect(withoutRoleTags('hi @fast')).toBe('hi')
    expect(withoutRoleTags('@FAST hi')).toBe('hi')
  })

  it('keeps a line break and leaves a message with no tag exactly as it was', () => {
    expect(withoutRoleTags('line one\n@fast line two')).toBe('line one\nline two')
    expect(withoutRoleTags('  keep   the  spacing ')).toBe('  keep   the  spacing ')
  })

  it('does not take a word that only contains a tag for one', () => {
    expect(withoutRoleTags('mail me@fast.com')).toBe('mail me@fast.com')
    expect(withoutRoleTags('the @fastest way')).toBe('the @fastest way')
  })
})

describe('withoutTagsInLastUserMessage', () => {
  it('changes only the last user message', () => {
    const history = [
      { role: 'user' as const, content: '@fast first' },
      { role: 'assistant' as const, content: 'ok' },
      { role: 'user' as const, content: '@deep second' },
    ]
    const out = withoutTagsInLastUserMessage(history)
    expect(out.map((m) => m.content)).toEqual(['@fast first', 'ok', 'second'])
    expect(history[2].content).toBe('@deep second') // the stored conversation is untouched
  })

  it('returns a conversation with no user message as it is', () => {
    const only = [{ role: 'system' as const, content: '@fast' }]
    expect(withoutTagsInLastUserMessage(only)).toBe(only)
  })
})

describe('suitsRole', () => {
  it('gives the three brains to language models only', () => {
    for (const role of ['fast', 'main', 'deep']) {
      expect(suitsRole(role, 'qwen3-8b', true)).toBe(true)
      expect(suitsRole(role, 'nomic-embed-text', false)).toBe(false)
      expect(suitsRole(role, 'whisper-base', false)).toBe(false)
    }
  })

  it('gives embed and rerank to the models named for them, and never an embedder to rerank', () => {
    expect(suitsRole('embed', 'nomic-embed-text', false)).toBe(true)
    expect(suitsRole('embed', 'bge-m3', false)).toBe(true)
    expect(suitsRole('embed', 'bge-reranker-v2-m3', false)).toBe(false)
    expect(suitsRole('rerank', 'bge-reranker-v2-m3', false)).toBe(true)
    expect(suitsRole('rerank', 'nomic-embed-text', false)).toBe(false)
    expect(suitsRole('embed', 'qwen3-8b', true)).toBe(false)
  })
})

describe('calls to the hub', () => {
  it('reads the roles with router.status', async () => {
    const m = hub({ model: AUTO, enabled: true, roles: [], support: [], overrides: {}, free_ram_mb: 1 })
    expect((await routerStatus()).model).toBe('auto')
    expect(sent(m).url).toBe('/v1/call/router.status')
  })

  it('asks which model a conversation would go to, pictures in the OpenAI shape', async () => {
    const m = hub({ role: 'main', model: 'qwen3-9b', forced: null, reasons: [], candidates: [] })
    const p = await pickFor([{ role: 'user', content: 'what is this?', images: ['data:image/png;base64,AAAA'] }])
    expect(p.model).toBe('qwen3-9b')
    const { url, body } = sent(m)
    expect(url).toBe('/v1/call/router.pick')
    expect(body.messages[0].content).toEqual([
      { type: 'text', text: 'what is this?' },
      { type: 'image_url', image_url: { url: 'data:image/png;base64,AAAA' } },
    ])
  })

  it('tests one typed message, and says when it comes with a picture', async () => {
    const m = hub({ role: 'fast', model: 'q', forced: null, reasons: [], candidates: [] })
    await pickText('hello')
    await pickText('look', true)
    expect(sent(m, 0).body).toEqual({ input: 'hello' })
    expect(sent(m, 1).body).toEqual({ input: 'look', has_image: true })
  })

  it('chooses a role with a merge patch, and clears it with null', async () => {
    const m = hub({})
    await setRole('deep', 'qwen3-27b')
    await setRole('deep', null)
    expect(sent(m, 0)).toEqual({ url: '/v1/call/misty.config.set', body: { patch: { roles: { deep: 'qwen3-27b' } } } })
    expect(sent(m, 1).body).toEqual({ patch: { roles: { deep: null } } })
  })
})

describe('routeLine', () => {
  it('names the role and the model', () => {
    expect(routeLine({ role: 'main', model: 'qwen3-8b', why: [] })).toBe('main: qwen3-8b')
  })
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

const route = vi.hoisted(() => ({ query: {} as Record<string, string> }))
vi.mock('vue-router', () => ({ useRoute: () => route, useRouter: () => ({ replace: vi.fn() }) }))
vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))
vi.mock('@/chat/hub', async (orig) => ({ ...(await orig<typeof import('@/chat/hub')>()), streamChat: vi.fn() }))

import { call } from '@/transport'
import { streamChat } from '@/chat/hub'
import { chats } from '@/chat/store'
import ChatView from './ChatView.vue'

const role = (r: string, model: string | null, over: Record<string, unknown> = {}) => ({
  role: r, model, status: model ? 'ready' : 'missing', source: 'preferred', loaded: false, note: '', ...over,
})
const MODELS = [
  { id: 'qwen3-2b', source: 'hf', size_gb: 1.5, deletable: true },
  { id: 'qwen3-9b', source: 'hf', size_gb: 6, deletable: true },
]
const status = (roles = [role('fast', 'qwen3-2b'), role('main', 'qwen3-9b'), role('deep', null)]) => ({
  model: 'auto', enabled: true, roles, support: [], overrides: {}, free_ram_mb: 8192,
})

let routerAnswer: any
let statusAnswer: any
const sent = (op: string) => vi.mocked(call).mock.calls.filter((c) => c[0] === op).map((c) => c[1] as any)

beforeEach(() => {
  localStorage.clear()
  chats.list.splice(0)
  route.query = {}
  routerAnswer = { role: 'deep', model: 'qwen3-9b', forced: 'deep', reasons: ['you asked for deep'], candidates: [] }
  statusAnswer = status()
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string) => {
    switch (op) {
      case 'model.list': return MODELS
      case 'router.status': return statusAnswer
      case 'router.pick': return routerAnswer
      default: throw new Error(`no operation ${op}`)
    }
  })
  vi.mocked(streamChat).mockReset()
  vi.mocked(streamChat).mockImplementation(async (_model, _history, _params, _trace, _signal, h) => {
    h.onContent('Here is the proof.')
    return { ttft_ms: 10, total_ms: 20, prompt_tokens: 5, completion_tokens: 4, tps: 100 }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
})

async function open() {
  wrapper = mount(ChatView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' }, ConfirmDialog: true } } })
  await flushPromises()
  return wrapper
}
async function say(w: VueWrapper, text: string) {
  await w.find('textarea[aria-label="Message"]').setValue(text)
  await w.find('form.composer').trigger('submit')
  await flushPromises()
}
const picker = (w: VueWrapper) => w.find('#modelpick').element as HTMLSelectElement

describe('Auto in a chat', () => {
  it('lists the router first and starts a new chat on it', async () => {
    const w = await open()
    const groups = w.findAll('#modelpick optgroup').map((g) => g.attributes('label'))
    expect(groups).toEqual(['Router', 'Local models'])
    expect(picker(w).value).toBe('auto')
    expect(w.text()).toContain('The router chooses between')
    expect(w.text()).toContain('fast: qwen3-2b')
  })

  it('does not start on the router when no role has a model that can answer', async () => {
    statusAnswer = status([role('fast', null), role('main', null), role('deep', null)])
    const w = await open()
    expect(picker(w).value).toBe('qwen3-2b') // a model, not Auto
    expect(w.findAll('#modelpick optgroup').map((g) => g.attributes('label'))).toContain('Router') // still there to choose
  })

  it('asks the router first, then sends the message to the model it chose without the role tag', async () => {
    const w = await open()
    await say(w, '@deep prove it')

    expect(sent('router.pick')).toHaveLength(1)
    const asked = sent('router.pick')[0].messages
    expect(asked[asked.length - 1]).toEqual({ role: 'user', content: '@deep prove it' })

    const [model, history] = vi.mocked(streamChat).mock.calls[0]
    expect(model).toBe('qwen3-9b')
    expect(history[history.length - 1]).toEqual({ role: 'user', content: 'prove it' })
  })

  it('says which model answered and why', async () => {
    const w = await open()
    await say(w, '@deep prove it')
    const reply = w.findAll('article.msg').find((a) => a.classes('assistant'))!
    expect(reply.text()).toContain('qwen3-9b')
    expect(reply.text()).toContain('router: deep')
    expect(reply.text()).toContain('Why the router chose qwen3-9b')
    expect(reply.text()).toContain('you asked for deep')
    expect(reply.text()).toContain('Here is the proof.')
  })

  it('refreshes the router strip after a reply', async () => {
    const w = await open()
    const before = sent('router.status').length
    await say(w, 'hello')
    expect(sent('router.status').length).toBeGreaterThan(before)
  })

  it('shows why when the router has no model for the message, and sends nothing', async () => {
    routerAnswer = { role: null, model: null, forced: null, reasons: ['this conversation has a picture and no model can see it'], candidates: [] }
    const w = await open()
    await say(w, 'what is this?')
    expect(w.find('.msgs .error-box').text()).toContain('The router has no model to send this to')
    expect(w.find('.msgs .error-box').text()).toContain('no model can see it')
    expect(streamChat).not.toHaveBeenCalled()
  })

  it('does not ask the router when a model is chosen by hand', async () => {
    const w = await open()
    await w.find('#modelpick').setValue('qwen3-2b')
    await say(w, 'hello')
    expect(sent('router.pick')).toHaveLength(0)
    expect(vi.mocked(streamChat).mock.calls[0][0]).toBe('qwen3-2b')
    const reply = w.findAll('article.msg').find((a) => a.classes('assistant'))!
    expect(reply.text()).not.toContain('router:')
  })

  it('is not offered by a hub that has no router', async () => {
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) => {
      if (op === 'router.status') throw new Error('no operation router.status')
      return real(op, args)
    })
    const w = await open()
    expect(w.findAll('#modelpick optgroup').map((g) => g.attributes('label'))).toEqual(['Local models'])
    expect(w.text()).not.toContain('The router chooses between')
  })
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

const route = vi.hoisted(() => ({ query: {} as Record<string, string> }))
vi.mock('vue-router', () => ({ useRoute: () => route, useRouter: () => ({ replace: vi.fn() }) }))
vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))
vi.mock('@/chat/hub', async (orig) => ({ ...(await orig<typeof import('@/chat/hub')>()), streamChat: vi.fn() }))
vi.mock('@/speech', async (orig) => ({
  ...(await orig<typeof import('@/speech')>()),
  startRecording: vi.fn(async () => ({ stop: async () => new Blob(['clip']) })),
  transcribeClip: vi.fn(async () => 'hello there'),
  speakClip: vi.fn(async () => new Blob(['wav'])),
}))

import { call } from '@/transport'
import { streamChat } from '@/chat/hub'
import { speakClip, startRecording, transcribeClip } from '@/speech'
import { chats } from '@/chat/store'
import ChatView from './ChatView.vue'

const STATS = { ttft_ms: 10, total_ms: 20, prompt_tokens: 5, completion_tokens: 4, tps: 100 }
const MODELS = [{ id: 'qwen3-2b', source: 'hf', size_gb: 1.5, deletable: true }]
const role = (r: string, model: string | null) => ({ role: r, model, status: model ? 'ready' : 'missing', source: 'preferred', loaded: false, note: '' })
const STATUS = { model: 'auto', enabled: true, roles: [role('fast', 'qwen3-2b'), role('main', null), role('deep', null)], support: [], overrides: {}, free_ram_mb: 8192 }

beforeEach(() => {
  localStorage.clear()
  // a chat on the router would ask router.pick first; these tests send to the model directly
  localStorage.setItem('mm_chat_model', 'qwen3-2b')
  chats.list.splice(0)
  route.query = {}
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string) => {
    switch (op) {
      case 'model.list': return MODELS
      case 'router.status': return STATUS
      case 'trace.list': return { items: [] }
      default: throw new Error(`no operation ${op}`)
    }
  })
  vi.mocked(streamChat).mockReset()
  vi.mocked(streamChat).mockImplementation(async (_m, _h, _p, _t, _s, h) => {
    h.onContent('Here is the proof.')
    return STATS
  })
  vi.mocked(startRecording).mockClear()
  vi.mocked(transcribeClip).mockClear()
  vi.mocked(speakClip).mockClear()
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
  vi.unstubAllGlobals()
})

async function open() {
  wrapper = mount(ChatView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' }, ConfirmDialog: true } } })
  await flushPromises()
  return wrapper
}
const box = (w: VueWrapper) => w.find('textarea[aria-label="Message"]')
async function say(w: VueWrapper, text: string) {
  await box(w).setValue(text)
  await w.find('form.composer').trigger('submit')
  await flushPromises()
}
const button = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text() === text)!

describe('a message written during a reply', () => {
  it('waits in the queue, shows as next, and is sent when the reply ends', async () => {
    let calls = 0
    let release = () => {}
    vi.mocked(streamChat).mockImplementation((async (_m: string, _h: unknown, _p: unknown, _t: unknown, _s: unknown, h: any) => {
      calls++
      if (calls === 1) {
        // the first reply goes on until the test lets it end
        return new Promise((ok) => {
          release = () => ok(STATS)
        })
      }
      h.onContent('second answer')
      return STATS
    }) as any)
    const w = await open()
    await say(w, 'first')
    await box(w).setValue('second')
    await w.find('form.composer').trigger('submit')
    await flushPromises()
    expect(w.find('.queued').text()).toContain('second')
    expect(calls).toBe(1)
    release()
    await flushPromises()
    await flushPromises()
    expect(calls).toBe(2)
    const sentSecond = vi.mocked(streamChat).mock.calls[1][1] as { role: string; content: string }[]
    expect(sentSecond[sentSecond.length - 1]).toMatchObject({ role: 'user', content: 'second' })
    expect(w.find('.queued').exists()).toBe(false)
  })

  it('can be taken back before it is sent', async () => {
    let release = () => {}
    vi.mocked(streamChat).mockImplementation((() =>
      new Promise((ok) => {
        release = () => ok(STATS)
      })) as any)
    const w = await open()
    await say(w, 'first')
    await box(w).setValue('never mind')
    await w.find('form.composer').trigger('submit')
    await w.find('button[aria-label="Remove queued message 1"]').trigger('click')
    release()
    await flushPromises()
    await flushPromises()
    expect(vi.mocked(streamChat)).toHaveBeenCalledTimes(1)
  })
})

describe('Jump to latest', () => {
  it('appears when the reader has scrolled up, and goes back to the newest message', async () => {
    const w = await open()
    const el = w.find('.msgs').element as HTMLElement
    Object.defineProperty(el, 'scrollHeight', { value: 1000, configurable: true })
    Object.defineProperty(el, 'clientHeight', { value: 200, configurable: true })
    Object.defineProperty(el, 'scrollTop', { value: 0, writable: true, configurable: true })
    await w.find('.msgs').trigger('scroll')
    expect(button(w, 'Jump to latest').exists()).toBe(true)
    await button(w, 'Jump to latest').trigger('click')
    await flushPromises()
    expect(w.findAll('button').some((b) => b.text() === 'Jump to latest')).toBe(false)
  })
})

describe('dictation', () => {
  it('writes what was said into the message box, after the second press', async () => {
    const w = await open()
    await button(w, 'Dictate').trigger('click')
    await flushPromises()
    expect(startRecording).toHaveBeenCalledTimes(1)
    expect(button(w, 'Stop listening').exists()).toBe(true)
    await button(w, 'Stop listening').trigger('click')
    await flushPromises()
    expect(transcribeClip).toHaveBeenCalledTimes(1)
    expect((box(w).element as HTMLTextAreaElement).value).toBe('hello there')
  })

  it('says so when nothing was heard', async () => {
    vi.mocked(transcribeClip).mockResolvedValueOnce('')
    const w = await open()
    await button(w, 'Dictate').trigger('click')
    await flushPromises()
    await button(w, 'Stop listening').trigger('click')
    await flushPromises()
    expect(w.find('.error-box').text()).toContain('No speech was recognised.')
  })
})

describe('read aloud', () => {
  it('speaks a reply with the hub voice, and plays it', async () => {
    URL.createObjectURL = vi.fn(() => 'blob:reply')
    URL.revokeObjectURL = vi.fn()
    const play = vi.fn(() => Promise.resolve())
    vi.stubGlobal(
      'Audio',
      class {
        onended: unknown = null
        play = play
        pause() {}
      },
    )
    const w = await open()
    await say(w, 'hi')
    await w.findAll('button').find((b) => b.text() === 'Read aloud')!.trigger('click')
    await flushPromises()
    expect(speakClip).toHaveBeenCalledWith('Here is the proof.')
    expect(play).toHaveBeenCalledTimes(1)
  })
})

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

// The microphone and the voice are replaced by stand-ins that the test drives: the microphone reports a sentence when the
// test says so, and the voice finishes a sentence when the test says so.
const rig = vi.hoisted(() => ({
  mics: [] as any[],
  speaks: [] as { sentence: string; done: (failure?: unknown) => void }[],
  /** What the browser reports for echo cancellation on the microphone (null: it does not say). */
  echoCancelling: null as boolean | null,
}))
vi.mock('@/live/audio', () => ({
  LiveMic: class {
    open = vi.fn(async () => undefined)
    setEnabled = vi.fn()
    close = vi.fn()
    echoCancelling: boolean | null = rig.echoCancelling
    constructor(public handlers: { onUtterance: (clip: Blob, ms: number) => void }) {
      rig.mics.push(this)
    }
  },
  LiveVoice: class {
    say = vi.fn((sentence: string, done: (failure?: unknown) => void) => {
      rig.speaks.push({ sentence, done })
    })
    stop = vi.fn()
  },
}))
vi.mock('@/speech', () => ({ transcribeClip: vi.fn(), notInBuild: () => false, speakClip: vi.fn() }))
vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn(async () => ({})) }))

import { call } from '@/transport'
import { transcribeClip } from '@/speech'
import LiveOverlay from './LiveOverlay.vue'

let wrapper: VueWrapper | undefined
let send: ReturnType<typeof vi.fn>
let stop: ReturnType<typeof vi.fn>

beforeEach(() => {
  rig.mics.length = 0
  rig.speaks.length = 0
  rig.echoCancelling = null
  vi.mocked(call).mockClear()
  vi.mocked(transcribeClip).mockReset()
  send = vi.fn(() => true)
  stop = vi.fn()
})
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
})

async function open() {
  wrapper = mount(LiveOverlay, { props: { send, stop, canPicture: false } as any })
  await flushPromises()
  return wrapper
}
const button = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text() === text)!
/** The owner says one sentence: the microphone reports it, and the hub's speech to text writes it down. */
async function hear(text: string) {
  vi.mocked(transcribeClip).mockResolvedValueOnce(text)
  rig.mics[0].handlers.onUtterance(new Blob(['x']), 900)
  await flushPromises()
}
/** A sentence the owner says, which waits to be sent (a held sentence). */
async function say(w: VueWrapper, text: string) {
  await hear(text)
  await vi.waitFor(() => expect(w.find('textarea[aria-label="Sentence about to be sent"]').exists()).toBe(true))
}

describe('the Live conversation', () => {
  it('holds a sentence the owner says, shows it, and sends it when the hold ends', async () => {
    const w = await open()
    expect(rig.mics[0]).toBeDefined()
    await say(w, 'what time is it')
    expect((w.find('textarea[aria-label="Sentence about to be sent"]').element as HTMLTextAreaElement).value).toBe('what time is it')
    expect(send).not.toHaveBeenCalled()
    await vi.waitFor(() => expect(send).toHaveBeenCalledWith('what time is it', []), { timeout: 3000 })
    // the chat is writing the answer now
    expect(w.text()).toContain('Thinking')
  })

  it('sends at once with Send now, and takes a held sentence back with Cancel send', async () => {
    const w = await open()
    await say(w, 'the tv says so')
    await button(w, 'Cancel send').trigger('click')
    await flushPromises()
    expect(w.find('textarea[aria-label="Sentence about to be sent"]').exists()).toBe(false)

    await say(w, 'turn it off')
    await button(w, 'Send now').trigger('click')
    await flushPromises()
    expect(send).toHaveBeenCalledTimes(1)
    expect(send).toHaveBeenCalledWith('turn it off', [])
  })

  it('reads the reply aloud, and saves a correction spoken against it through misty.teach', async () => {
    const w = await open()
    await say(w, 'what is the capital of spain')
    await button(w, 'Send now').trigger('click')
    await flushPromises()

    // the chat writes the reply, and the overlay says it sentence by sentence
    const live = w.vm as unknown as { reply: (text: string | null, busy: boolean, error?: string | null) => void }
    live.reply('Lyon is the capital.', true)
    live.reply('Lyon is the capital.', false)
    await flushPromises()
    expect(rig.speaks.map((s) => s.sentence)).toEqual(['Lyon is the capital.'])
    rig.speaks[0].done()
    await flushPromises()

    await hear("that's wrong, it is Madrid")
    await flushPromises()
    expect(call).toHaveBeenCalledWith('misty.teach', {
      question: 'what is the capital of spain',
      answer: 'Lyon is the capital.',
      correction: 'it is Madrid',
    })
    expect(w.text()).toContain('Saved. Misty uses it for similar questions.')
  })

  it('stops the reply that is being written, and the microphone, when the owner presses End', async () => {
    const w = await open()
    await say(w, 'tell me a long story')
    await button(w, 'Send now').trigger('click')
    await flushPromises()
    expect(stop).not.toHaveBeenCalled()
    await button(w, 'End').trigger('click')
    await flushPromises()
    expect(stop).toHaveBeenCalledTimes(1)
    expect(w.emitted('close')).toHaveLength(1)
    // the component is unmounted by its parent after close: the microphone goes quiet then
    wrapper?.unmount()
    wrapper = undefined
    expect(rig.mics[0].close).toHaveBeenCalled()
  })

  it('says so when the browser is not taking the reply out of the microphone', async () => {
    rig.echoCancelling = false
    await open()
    expect(wrapper!.text()).toContain('A headset avoids that.')
  })

  it('says nothing about echo when the browser reports it on, or does not say', async () => {
    rig.echoCancelling = true
    await open()
    expect(wrapper!.text()).not.toContain('A headset avoids that.')
    wrapper!.unmount()
    wrapper = undefined
    rig.echoCancelling = null
    await open()
    expect(wrapper!.text()).not.toContain('A headset avoids that.')
  })
})

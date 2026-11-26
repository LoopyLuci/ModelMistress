import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))

import { call } from '@/transport'
import MistySystemView from './MistySystemView.vue'

const STATUS = {
  ready: true,
  roster: [],
  engines: { stock_q2: false, prism: false, llama_version: null },
  cactus: { available: false, library: null, models: [], error: null, archives: [] },
  free_ram_mb: 1000,
  rag: { collections: [] },
  policy: {},
  roots: [],
  runs_active: 0,
  approvals_pending: 0,
  next: [],
}

let verdicts: any[]
const sent = (op: string) => vi.mocked(call).mock.calls.filter((c) => c[0] === op).map((c) => c[1] as any)
const counts = () => ({
  total: verdicts.length,
  good: verdicts.filter((v) => v.label === 'good').length,
  bad: 0,
  no_tools: verdicts.filter((v) => v.label === 'no_tools').length,
  needs_tools: 0,
  corrected: verdicts.filter((v) => v.label === 'corrected').length,
  training: verdicts.length,
})

beforeEach(() => {
  verdicts = [
    { id: 7, question: 'capital of france', answer: 'Lyon', label: 'corrected', tools: [], correction: 'Paris', at: 1 },
    { id: 8, question: 'what time is it', answer: 'noon', label: 'no_tools', tools: [], correction: null, at: 2 },
  ]
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string, args: any) => {
    switch (op) {
      case 'misty.status': return STATUS
      case 'job.list': return []
      case 'misty.feedback.list': return { counts: counts(), verdicts }
      case 'misty.feedback.forget':
        if (args.all) {
          const n = verdicts.length
          verdicts = []
          return { removed: n }
        }
        verdicts = verdicts.filter((v) => v.id !== args.id)
        return { removed: 1 }
      case 'misty.training.export': return { count: verdicts.length, jsonl: '{"messages":[]}\n' }
      default: throw new Error(`no operation ${op}`)
    }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
  vi.restoreAllMocks()
})

async function openLearning() {
  wrapper = mount(MistySystemView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' } } } })
  await flushPromises()
  await wrapper.findAll('[role="tab"]').find((t) => t.text() === 'Learning')!.trigger('click')
  await flushPromises()
  return wrapper
}

describe('what Misty has learned from the ratings', () => {
  it('shows the counts and each rating, with the right answer for a correction', async () => {
    const w = await openLearning()
    expect(sent('misty.feedback.list')[0]).toEqual({ limit: 200 })
    const kv = w.find('dl.kv').text()
    expect(kv).toContain('ratings')
    expect(kv).toContain('corrections')
    const rows = w.findAll('.verdicts li')
    expect(rows).toHaveLength(2)
    expect(rows[0].text()).toContain('capital of france')
    expect(rows[0].text()).toContain('right answer: Paris')
  })

  it('forgets one rating only after the owner confirms', async () => {
    const w = await openLearning()
    vi.spyOn(window, 'confirm').mockReturnValue(false)
    await w.find('button[aria-label="Forget the rating of capital of france"]').trigger('click')
    await flushPromises()
    expect(sent('misty.feedback.forget')).toEqual([])
    vi.mocked(window.confirm).mockReturnValue(true)
    await w.find('button[aria-label="Forget the rating of capital of france"]').trigger('click')
    await flushPromises()
    expect(sent('misty.feedback.forget')).toEqual([{ id: 7 }])
    expect(w.findAll('.verdicts li')).toHaveLength(1)
  })

  it('forgets every rating after one confirmation', async () => {
    const w = await openLearning()
    vi.spyOn(window, 'confirm').mockReturnValue(true)
    await w.findAll('button').find((b) => b.text() === 'Forget all ratings')!.trigger('click')
    await flushPromises()
    expect(sent('misty.feedback.forget')).toEqual([{ all: true }])
    expect(w.findAll('.verdicts li')).toHaveLength(0)
  })

  it('exports the training examples as a JSON lines file', async () => {
    const w = await openLearning()
    const made = vi.fn(() => 'blob:misty')
    const revoked = vi.fn()
    URL.createObjectURL = made
    URL.revokeObjectURL = revoked
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})
    await w.findAll('button').find((b) => b.text().startsWith('Export training data'))!.trigger('click')
    await flushPromises()
    expect(sent('misty.training.export')).toHaveLength(1)
    expect(made).toHaveBeenCalledTimes(1)
    expect(click).toHaveBeenCalledTimes(1)
    expect(w.text()).toContain('Exported 2 example(s).')
  })
})

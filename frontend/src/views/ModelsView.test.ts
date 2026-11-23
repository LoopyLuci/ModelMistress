import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

const route = vi.hoisted(() => ({ query: {} as Record<string, string> }))
vi.mock('vue-router', () => ({ useRoute: () => route }))
vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))

import { call } from '@/transport'
import ModelsView from './ModelsView.vue'

const role = (r: string, model: string | null, over: Record<string, unknown> = {}) => ({
  role: r, model, status: model ? 'ready' : 'missing', source: 'preferred', loaded: false, note: '', ...over,
})
const MODELS = [
  { id: 'qwen3-8b', source: 'hf', size_gb: 5, loaded: true, deletable: true, architecture: 'qwen3', file_type: 'Q4_K_M' },
  { id: 'llama-3b', source: 'hf', size_gb: 2, loaded: false, deletable: false, architecture: 'llama', file_type: 'Q8_0' },
  { id: 'nomic-embed-text', source: 'hf', size_gb: 0.1, loaded: false, deletable: true, architecture: 'nomic-bert' },
]
const STATUS = {
  model: 'auto',
  enabled: true,
  roles: [role('fast', 'llama-3b'), role('main', 'qwen3-8b', { source: 'config', loaded: true }), role('deep', null)],
  support: [role('embed', 'nomic-embed-text', { source: 'fallback' }), role('rerank', null)],
  overrides: { main: 'qwen3-8b' } as Record<string, string>,
  free_ram_mb: 8192,
}
const PICK = {
  role: 'fast', model: 'llama-3b', forced: null, reasons: ['a short message'],
  candidates: [{ ...role('fast', 'llama-3b'), score: 0.8 }],
}

let profiles: Record<string, unknown>
const sent = (op: string) => vi.mocked(call).mock.calls.filter((c) => c[0] === op).map((c) => c[1] as any)

beforeEach(() => {
  route.query = {}
  profiles = { 'nomic-embed-text': { engine: 'prism', batch_size: 2048 } }
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string, args: any = {}) => {
    switch (op) {
      case 'model.list': return MODELS
      case 'model.loaded': return []
      case 'job.list': return []
      case 'router.status': return STATUS
      case 'router.pick': return PICK
      case 'config.get': return { profiles }
      case 'config.set': profiles = args.settings.profiles; return { profiles }
      case 'misty.config.set': return {}
      case 'model.plan': return { verdict: 'fits', estimate: { total_mib: 5000 }, free_mib: 20000, unload: [] }
      case 'model.load': return { load_ms: 1500 }
      default: throw new Error(`no operation ${op}`)
    }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount() // stops the page's polling
  wrapper = undefined
})

async function open(tab = '') {
  route.query = tab ? { tab } : {}
  wrapper = mount(ModelsView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' }, ConfirmDialog: true } } })
  await flushPromises()
  return wrapper
}
const card = (w: VueWrapper, id: string) => w.findAll('.model').find((c) => c.find('.name').text() === id)!
const button = (root: { findAll: VueWrapper['findAll'] }, text: string) => root.findAll('button').find((b) => b.text() === text)!

describe('the installed models', () => {
  it('shows which router role a model has and which have their own settings', async () => {
    const w = await open()
    expect(card(w, 'qwen3-8b').text()).toContain('router: main')
    expect(card(w, 'llama-3b').text()).toContain('router: fast')
    expect(card(w, 'nomic-embed-text').text()).toContain('router: embed')
    expect(card(w, 'nomic-embed-text').text()).toContain('custom settings')
    expect(card(w, 'qwen3-8b').text()).not.toContain('custom settings')
  })

  it('puts the embedding model in its own group, not with the image models', async () => {
    const w = await open()
    expect(w.text()).toContain('Embedding and reranking models')
    expect(w.text()).not.toContain('Speech, vision and image models')
  })

  it('does not offer to delete a model ModelMistress did not download', async () => {
    const w = await open()
    expect(button(card(w, 'llama-3b'), 'Delete').attributes('disabled')).toBeDefined()
    expect(button(card(w, 'qwen3-8b'), 'Delete').attributes('disabled')).toBeUndefined()
  })

  it('filters by name and by what the model does', async () => {
    const w = await open()
    await w.find('input[type=search]').setValue('llama')
    expect(w.findAll('.model').map((c) => c.find('.name').text())).toEqual(['llama-3b'])

    await w.find('input[type=search]').setValue('')
    await w.find('select[aria-label="Show"]').setValue('custom')
    expect(w.findAll('.model').map((c) => c.find('.name').text())).toEqual(['nomic-embed-text'])

    await w.find('select[aria-label="Show"]').setValue('loaded')
    expect(w.findAll('.model').map((c) => c.find('.name').text())).toEqual(['qwen3-8b'])
  })

  it('sorts the largest first', async () => {
    const w = await open()
    await w.find('select[aria-label="Sort by"]').setValue('size')
    const local = w.findAll('.group').find((g) => g.text().includes('Local models'))!
    expect(local.findAll('.model .name').map((n) => n.text())).toEqual(['qwen3-8b', 'llama-3b'])
  })

  it('gives a model a role from its card', async () => {
    const w = await open()
    await w.find('select[id="use-llama-3b"]').setValue('deep')
    await flushPromises()
    expect(sent('misty.config.set')).toEqual([{ patch: { roles: { deep: 'llama-3b' } } }])
  })
})

describe('the fit check', () => {
  it('asks the hub whether a load fits and says so before loading', async () => {
    const w = await open()
    await button(card(w, 'llama-3b'), 'Load...').trigger('click')
    await w.find('input[placeholder="setting"]').setValue('4096')
    await new Promise((r) => setTimeout(r, 450)) // the page waits for the typing to stop
    await flushPromises()
    const plans = sent('model.plan')
    expect(plans[plans.length - 1]).toMatchObject({ model: 'llama-3b', ctx_size: 4096 })
    expect(card(w, 'llama-3b').text()).toContain('This fits.')
  })

  it('says so when it does not fit', async () => {
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) =>
      op === 'model.plan' ? { verdict: 'does_not_fit', estimate: { total_mib: 30000 }, free_mib: 8000, unload: [], reclaimable_mib: 2000 } : real(op, args),
    )
    const w = await open()
    await button(card(w, 'llama-3b'), 'Load...').trigger('click')
    await w.find('input[placeholder="setting"]').setValue('999999')
    await new Promise((r) => setTimeout(r, 450))
    await flushPromises()
    expect(card(w, 'llama-3b').text()).toContain('This does not fit.')
  })
})

describe('where a model goes', () => {
  /** Answers `model.plan` and `model.loaded` with these; the rest as in `beforeEach`. */
  function answer(over: Record<string, unknown>) {
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) => (op in over ? over[op] : real(op, args)))
  }
  const plan = (o: Record<string, unknown>) => ({ verdict: 'does_not_fit', estimate: { total_mib: 30000 }, free_mib: 8000, unload: [], ...o })
  async function openLoadForm(model: string) {
    const w = await open()
    await button(card(w, model), 'Load...').trigger('click')
    await w.find('input[placeholder="setting"]').setValue('4096')
    await new Promise((r) => setTimeout(r, 450))
    await flushPromises()
    return w
  }

  it('says that part of a model that is too big for the GPU runs from system memory', async () => {
    answer({ 'model.plan': plan({ placement: { where: 'split', gpu_share: 0.57 } }) })
    const w = await openLoadForm('llama-3b')
    expect(card(w, 'llama-3b').text()).toContain('About 57% of it goes on the GPU and the rest runs from system memory')
    expect(card(w, 'llama-3b').text()).not.toContain('This does not fit')
  })

  it('says so when it runs on the CPU, and when it fits neither', async () => {
    answer({ 'model.plan': plan({ placement: { where: 'cpu' } }) })
    expect((await openLoadForm('llama-3b')).text()).toContain('runs on the CPU')
    wrapper?.unmount()
    answer({ 'model.plan': plan({ placement: { where: 'none', need_ram_mib: 40000, free_ram_mib: 8000 } }) })
    const w = await openLoadForm('llama-3b')
    expect(card(w, 'llama-3b').text()).toContain('This does not fit this machine')
    expect(card(w, 'llama-3b').text()).toContain('39.1 GB of system memory')
  })

  it('starts a model that crashed the GPU on the CPU, and lets the owner try the GPU again', async () => {
    answer({ 'model.plan': plan({ verdict: 'fits', learned_cpu: 'its load on the GPU crashed llama-server (exit 0xC0000409)' }) })
    const w = await openLoadForm('llama-3b')
    const c = card(w, 'llama-3b')
    expect(c.text()).toContain('It starts on the CPU: its load on the GPU crashed llama-server')
    const retry = c.findAll('label').find((l) => l.text().includes('Try the GPU again'))!
    await retry.find('input').setValue(true)
    await c.find('form.loadform').trigger('submit')
    await flushPromises()
    expect(sent('model.load')).toEqual([{ model: 'llama-3b', ctx_size: 4096, retry_gpu: true }])
  })

  it('does not offer another GPU try for a model that has no crash on record', async () => {
    const w = await openLoadForm('llama-3b')
    expect(card(w, 'llama-3b').text()).not.toContain('Try the GPU again')
    await card(w, 'llama-3b').find('form.loadform').trigger('submit')
    await flushPromises()
    expect(sent('model.load')[0]).not.toHaveProperty('retry_gpu')
  })

  const loaded = (o: Record<string, unknown>) => ({ model: 'qwen3-8b', pid: 1, port: 2, uptime_s: 5, idle_s: 1, embeddings: false, reranking: false, ...o })

  it('tells where a loaded model runs from what was measured, and why when it is not where it was asked', async () => {
    answer({
      'model.loaded': [
        loaded({
          args: ['-c', '8192', '--fit', 'on', '--fit-target', '768', '-np', '2'],
          serving: { placement: 'partial' },
        }),
        loaded({
          model: 'nomic-embed-text',
          args: ['-c', '8192', '-ngl', '0', '-np', '2', '-dev', 'none'],
          serving: { placement: 'cpu', placement_note: 'its load on the GPU crashed llama-server (exit 0xC0000409); it runs on the CPU' },
        }),
      ],
    })
    const w = await open()
    const rows = w.findAll('.loadedrow')
    expect(rows[0].text()).toContain('partly on the GPU')
    expect(rows[1].text()).toContain('CPU only')
    expect(rows[1].text()).toContain('its load on the GPU crashed llama-server (exit 0xC0000409); it runs on the CPU')
    expect(rows[0].text()).not.toContain('NaN')
  })

  it('does not print NaN for a server that chose its own layers and has not been measured', async () => {
    answer({ 'model.loaded': [loaded({ args: ['-c', '8192', '--fit', 'on', '-np', '2'] })] })
    const w = await open()
    expect(w.find('.loadedrow').text()).toContain('GPU layers chosen by llama-server')
    expect(w.find('.loadedrow').text()).not.toContain('NaN')
  })
})

describe('the settings of one model', () => {
  const field =(w: VueWrapper, label: string) => w.findAll('label.field').find((l) => l.text().startsWith(label))!

  it('shows the saved profile and saves a change with the rest of the table', async () => {
    const w = await open()
    await button(card(w, 'nomic-embed-text'), 'Settings').trigger('click')
    expect((field(w, 'Batch size').find('input').element as HTMLInputElement).value).toBe('2048')
    expect((field(w, 'Engine').find('select').element as HTMLSelectElement).value).toBe('prism')

    await field(w, 'Batch size').find('input').setValue('4096')
    await field(w, 'Micro-batch size').find('input').setValue('4096')
    await w.find('form.loadform').trigger('submit')
    await flushPromises()

    expect(sent('config.set')).toEqual([
      { settings: { profiles: { 'nomic-embed-text': { engine: 'prism', batch_size: 4096, ubatch_size: 4096 } } } },
    ])
    expect(w.text()).toContain('Settings for nomic-embed-text saved.')
  })

  it('shows the hub reason when it refuses a value', async () => {
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) => {
      if (op === 'config.set') throw new Error('profiles."qwen3-8b".cache_type_k: unknown type zz')
      return real(op, args)
    })
    const w = await open()
    await button(card(w, 'qwen3-8b'), 'Settings').trigger('click')
    await field(w, 'KV cache type (keys)').find('select').setValue('q8_0')
    await w.find('form.loadform').trigger('submit')
    await flushPromises()
    expect(w.find('.error-box').text()).toContain('unknown type zz')
  })

  it('removes the profile on reset', async () => {
    const w = await open()
    await button(card(w, 'nomic-embed-text'), 'Settings').trigger('click')
    await button(w.find('form.loadform'), 'Reset to the global settings').trigger('click')
    await flushPromises()
    expect(sent('config.set')).toEqual([{ settings: { profiles: {} } }])
  })
})

describe('the router tab', () => {
  it('opens from a link and lists every role with its model and state', async () => {
    const w = await open('router')
    const rows = w.findAll('table.roles tbody tr')
    expect(rows.map((r) => r.find('strong').text())).toEqual(['fast', 'main', 'deep', 'embed', 'rerank'])
    expect(rows[0].text()).toContain('ready')
    expect(rows[1].text()).toContain('loaded')
    expect(rows[2].text()).toContain('no model')
    expect(w.text()).toContain('8.0 GB') // the free memory the router counts on
  })

  it('offers only models that suit the role', async () => {
    const w = await open('router')
    const options = (r: string) => w.findAll(`select[aria-label="Model for ${r}"] option`).map((o) => o.attributes('value'))
    expect(options('main')).toEqual(['', 'llama-3b', 'qwen3-8b'])
    expect(options('embed')).toEqual(['', 'nomic-embed-text'])
    expect(options('rerank')).toEqual([''])
  })

  it('chooses a model for a role and goes back to automatic', async () => {
    const w = await open('router')
    await w.find('select[aria-label="Model for fast"]').setValue('qwen3-8b')
    await flushPromises()
    await w.find('select[aria-label="Model for main"]').setValue('')
    await flushPromises()
    expect(sent('misty.config.set')).toEqual([
      { patch: { roles: { fast: 'qwen3-8b' } } },
      { patch: { roles: { main: null } } },
    ])
  })

  it('loads the model a role has', async () => {
    const w = await open('router')
    await button(w.findAll('table.roles tbody tr')[0], 'Load now').trigger('click')
    await flushPromises()
    expect(sent('model.load')).toEqual([{ model: 'llama-3b' }])
  })

  it('shows where a message would go, and why', async () => {
    const w = await open('router')
    await w.find('input[aria-label="Message to test"]').setValue('hello')
    await w.find('form[aria-label="Try the router"]').trigger('submit')
    await flushPromises()
    expect(sent('router.pick')).toEqual([{ input: 'hello' }])
    expect(w.find('.tryout').text()).toContain('Goes to llama-3b')
    expect(w.find('.tryout').text()).toContain('a short message')
  })

  it('says so when the hub has no router', async () => {
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) => {
      if (op === 'router.status') throw new Error('no operation router.status')
      return real(op, args)
    })
    const w = await open('router')
    expect(w.text()).toContain('not available on this hub')
  })
})

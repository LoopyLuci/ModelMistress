import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))

import { call } from '@/transport'
import FolderPicker from '@/components/FolderPicker.vue'
import MistyView from './MistyView.vue'

const POLICY = { read: 'auto', write: 'ask', exec: 'deny', net: 'deny', hub: 'ask' }
const APP = { id: 'e1', name: 'My app', description: '', cwd: '/work/app', roots: [], vars: {}, policy: { exec: 'ask' }, created: 1, updated: 1 }
const DOCS = { id: 'e2', name: 'Docs', description: '', cwd: null, roots: [], vars: {}, policy: {}, created: 1, updated: 1 }

let envList: unknown[]
let sessions: any[]
let envsFail = false
const sent = (op: string) => vi.mocked(call).mock.calls.filter((c) => c[0] === op).map((c) => c[1] as any)

beforeEach(() => {
  localStorage.clear()
  envList = [APP, DOCS]
  envsFail = false
  sessions = [
    { id: 's1', title: 'old chat', created: 1, updated: 2, messages: 2, env: 'e1', env_name: 'My app', cwd: null },
    { id: 's2', title: 'own folder', created: 1, updated: 1, messages: 2, env: null, env_name: null, cwd: '/somewhere/else' },
  ]
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string, args: any) => {
    switch (op) {
      case 'misty.status': return { ready: true, roster: [], policy: POLICY, roots: [] }
      case 'misty.env.list':
        if (envsFail) throw new Error('no operation misty.env.list')
        return { environments: envList }
      case 'misty.session.list': return { sessions }
      case 'misty.session.get': return { messages: [{ role: 'user', content: 'what is in main.rs?' }, { role: 'assistant', content: 'a main function' }] }
      case 'misty.session.set': {
        // the hub keeps what it is told
        const s = sessions.find((x) => x.id === args.session)
        if ('cwd' in args) s.cwd = args.cwd
        if ('env' in args) {
          s.env = args.env
          s.env_name = (envList as any[]).find((e) => e.id === args.env)?.name ?? null
        }
        return { session: args.session }
      }
      case 'misty.run.start': return { run: 'r1', session: args.session || 'made' }
      case 'misty.run.get': return { run: 'r1', events: [{ type: 'final', status: 'done', text: 'ok', ms: 5 }], next: 1, done: true, running: false, pending: [] }
      default: throw new Error(`no operation ${op}`)
    }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
})

async function open() {
  wrapper = mount(MistyView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' }, FolderPicker: true } } })
  await flushPromises()
  return wrapper
}
const envPick = (w: VueWrapper) => w.find('select[aria-label="Environment"]')
const box = (w: VueWrapper) => w.find('textarea[aria-label="message"]')
const folderText = (w: VueWrapper) => w.find('.scope .folder').text()

describe('where Misty works', () => {
  it('lists the environments and says what each class of tool does in the chosen one', async () => {
    const w = await open()
    const names = w.findAll('select[aria-label="Environment"] option').map((o) => o.text())
    expect(names).toEqual(["none: Misty's own settings", 'My app', 'Docs'])
    expect(folderText(w)).toBe("Misty's own folder")
    expect(w.find('.can').text()).toContain('commands: deny')
    await envPick(w).setValue('e1')
    expect(folderText(w)).toBe('/work/app')
    expect(w.find('.scope .pill').text()).toBe('environment')
    // the environment's own choice wins over Misty's setting
    expect(w.find('.can').text()).toContain('commands: ask')
    expect(w.find('.can').text()).toContain('write: ask')
  })

  it('starts a new chat where the last one worked, and passes that on with the first message', async () => {
    localStorage.setItem('mm_misty_env', 'e1')
    localStorage.setItem('mm_misty_cwd', '/work/other')
    const w = await open()
    expect((envPick(w).element as HTMLSelectElement).value).toBe('e1')
    expect(folderText(w)).toBe('/work/other') // the chat's own folder wins
    await box(w).setValue('list the files')
    await w.find('form.compose').trigger('submit')
    await flushPromises()
    const start = sent('misty.run.start')[0]
    expect(start).toMatchObject({ input: 'list the files', new_session: true, env: 'e1', cwd: '/work/other' })
  })

  it('sends neither when none is chosen, and forgets an environment that no longer exists', async () => {
    localStorage.setItem('mm_misty_env', 'gone')
    const w = await open()
    expect((envPick(w).element as HTMLSelectElement).value).toBe('')
    await box(w).setValue('hello')
    await w.find('form.compose').trigger('submit')
    await flushPromises()
    const start = sent('misty.run.start')[0]
    expect(start.env).toBeUndefined()
    expect(start.cwd).toBeUndefined()
  })

  it('shows the environment and folder an opened chat has, and does not send them again', async () => {
    const w = await open()
    await w.findAll('.sessions .link')[1].trigger('click') // "own folder"
    await flushPromises()
    expect((envPick(w).element as HTMLSelectElement).value).toBe('')
    expect(folderText(w)).toBe('/somewhere/else')
    expect(w.find('.scope .pill').text()).toBe('this chat')
    await box(w).setValue('and now?')
    await w.find('form.compose').trigger('submit')
    await flushPromises()
    const start = sent('misty.run.start')[0]
    expect(start.session).toBe('s2')
    expect(start.env).toBeUndefined()
    expect(start.cwd).toBeUndefined()
  })

  it('changes the environment of the chat that is open and remembers it for the next chat', async () => {
    const w = await open()
    await w.findAll('.sessions .link')[0].trigger('click')
    await flushPromises()
    expect((envPick(w).element as HTMLSelectElement).value).toBe('e1')
    await envPick(w).setValue('e2')
    await flushPromises()
    expect(sent('misty.session.set')).toEqual([{ session: 's1', env: 'e2', cwd: null }])
    expect(localStorage.getItem('mm_misty_env')).toBe('e2')
  })

  it('changes the folder from the picker and goes back to the environment with one click', async () => {
    sessions[0].cwd = null
    const w = await open()
    await w.findAll('.sessions .link')[0].trigger('click')
    await flushPromises()
    const change = w.findAll('.scope button').find((b) => b.text() === 'Change...')!
    await change.trigger('click')
    expect(w.findComponent(FolderPicker).props('open')).toBe(true)
    expect(w.findComponent(FolderPicker).props('start')).toBe('/work/app')
    w.findComponent(FolderPicker).vm.$emit('pick', '/work/new')
    await flushPromises()
    expect(sent('misty.session.set')[0]).toEqual({ session: 's1', env: 'e1', cwd: '/work/new' })
    expect(folderText(w)).toBe('/work/new')
    expect(w.find('.scope .pill').text()).toBe('this chat')
    const back = w.findAll('.scope button').find((b) => b.text().startsWith('Use the environment'))!
    await back.trigger('click')
    await flushPromises()
    expect(sent('misty.session.set')[1]).toEqual({ session: 's1', env: 'e1', cwd: null })
    expect(folderText(w)).toBe('/work/app')
  })

  it('says why when the hub refuses a change and shows what the hub has', async () => {
    const w = await open()
    await w.findAll('.sessions .link')[0].trigger('click')
    await flushPromises()
    const real = vi.mocked(call).getMockImplementation()!
    vi.mocked(call).mockImplementation(async (op: string, args: any) => {
      if (op === 'misty.session.set') throw new Error('cwd: there is no such folder')
      return real(op, args)
    })
    await envPick(w).setValue('e2')
    await flushPromises()
    expect(w.find('.error-box').text()).toContain('no such folder')
    expect((envPick(w).element as HTMLSelectElement).value).toBe('e1') // what the hub still has
  })

  it('has no environment bar on a hub from before environments, and chat still works', async () => {
    envsFail = true
    const w = await open()
    expect(w.find('.scope').exists()).toBe(false)
    await box(w).setValue('hi')
    await w.find('form.compose').trigger('submit')
    await flushPromises()
    expect(sent('misty.run.start')[0]).toMatchObject({ input: 'hi', new_session: true })
  })
})

describe('Up and Down in the Misty message box', () => {
  it('brings back what was sent in this session and then what was sent elsewhere', async () => {
    localStorage.setItem('mm_input_history', JSON.stringify(['from another chat']))
    const w = await open()
    await w.findAll('.sessions .link')[0].trigger('click')
    await flushPromises()
    const el = box(w).element as HTMLTextAreaElement
    await box(w).trigger('keydown', { key: 'ArrowUp' })
    expect(el.value).toBe('what is in main.rs?')
    await box(w).trigger('keydown', { key: 'ArrowUp' })
    expect(el.value).toBe('from another chat')
    await box(w).trigger('keydown', { key: 'ArrowDown' })
    expect(el.value).toBe('what is in main.rs?')
    await box(w).trigger('keydown', { key: 'ArrowDown' })
    expect(el.value).toBe('')
  })

  it('keeps a sent message for next time, and Ctrl+Enter still sends', async () => {
    const w = await open()
    await box(w).setValue('remember me')
    await box(w).trigger('keydown', { key: 'Enter', ctrlKey: true })
    await flushPromises()
    expect(sent('misty.run.start')[0].input).toBe('remember me')
    expect(JSON.parse(localStorage.getItem('mm_input_history') || '[]')).toEqual(['remember me'])
    await box(w).trigger('keydown', { key: 'ArrowUp' })
    expect((box(w).element as HTMLTextAreaElement).value).toBe('remember me')
  })
})

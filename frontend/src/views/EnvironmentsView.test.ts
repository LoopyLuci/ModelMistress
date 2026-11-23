import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))
const push = vi.hoisted(() => vi.fn())
vi.mock('vue-router', () => ({ useRouter: () => ({ push }) }))

import { call } from '@/transport'
import FolderPicker from '@/components/FolderPicker.vue'
import EnvironmentsView from './EnvironmentsView.vue'

const POLICY = { read: 'auto', write: 'ask', exec: 'deny', net: 'deny', hub: 'ask' }
const APP = {
  id: 'e1', name: 'My app', description: 'the app', cwd: '/work/app', roots: ['/work/data'], vars: { MODE: 'dev' },
  policy: { exec: 'ask' }, created: 1, updated: 1,
}

let envs: any[]
let saveError = ''
const sent = (op: string) => vi.mocked(call).mock.calls.filter((c) => c[0] === op).map((c) => c[1] as any)

beforeEach(() => {
  localStorage.clear()
  push.mockReset()
  saveError = ''
  envs = [structuredClone(APP)]
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string, args: any) => {
    switch (op) {
      case 'misty.status': return { policy: POLICY }
      case 'misty.env.list': return { environments: envs }
      case 'misty.env.save': {
        if (saveError) throw new Error(saveError)
        const e = { created: 1, updated: 2, ...args, id: args.id || `n${envs.length + 1}` }
        envs = args.id ? envs.map((x) => (x.id === args.id ? e : x)) : [...envs, e]
        return { environment: e }
      }
      case 'misty.env.delete':
        envs = envs.filter((x) => x.id !== args.id)
        return { deleted: true }
      default: throw new Error(`no operation ${op}`)
    }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
})

/** A confirm dialog that shows its text and a button while it is open, since jsdom has no modal dialogs. */
const ConfirmStub = {
  props: ['open', 'title', 'action'],
  emits: ['confirm', 'cancel'],
  template: '<div v-if="open" class="confirm-stub"><b>{{ title }}</b><slot /><button class="yes" @click="$emit(\'confirm\')">{{ action }}</button><button class="no" @click="$emit(\'cancel\')">cancel</button></div>',
}

async function open() {
  wrapper = mount(EnvironmentsView, { global: { stubs: { RouterLink: { template: '<a><slot /></a>' }, FolderPicker: true, ConfirmDialog: ConfirmStub } } })
  await flushPromises()
  return wrapper
}
const field = (w: VueWrapper, label: string) => w.find(`[aria-label="${label}"]`)
const nameBox = (w: VueWrapper) => w.find('input[maxlength="60"]')
const button = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text() === text)!
const policyOf = (w: VueWrapper, label: string) => w.find(`select[aria-label="${label} in this environment"]`)

describe('the list and the editor', () => {
  it('lists the environments with their folders and opens one to change', async () => {
    const w = await open()
    expect(w.find('.list').text()).toContain('My app')
    expect(w.find('.list').text()).toContain('/work/app')
    await w.find('.list .link').trigger('click')
    expect((nameBox(w).element as HTMLInputElement).value).toBe('My app')
    expect((field(w, 'Working folder').element as HTMLInputElement).value).toBe('/work/app')
    expect((field(w, 'Folder 1').element as HTMLInputElement).value).toBe('/work/data')
    expect((field(w, 'Variable 1 name').element as HTMLInputElement).value).toBe('MODE')
    // a value is hidden until shown
    expect(field(w, 'Variable 1 value').attributes('type')).toBe('password')
    await button(w, 'Show').trigger('click')
    expect(field(w, 'Variable 1 value').attributes('type')).toBe('text')
    expect((policyOf(w, 'Commands').element as HTMLSelectElement).value).toBe('ask')
    expect((policyOf(w, 'Write').element as HTMLSelectElement).value).toBe('')
    // the options say what Misty's own setting is
    expect(policyOf(w, 'Write').find('option').text()).toBe("Use Misty's setting (ask)")
    expect(button(w, 'Save changes').attributes('disabled')).toBeDefined() // nothing changed yet
  })

  it('says so when there are none', async () => {
    envs = []
    const w = await open()
    expect(w.find('.list').text()).toContain('None yet')
  })
})

describe('making and changing environments', () => {
  it('creates one with a folder, another folder, a variable and a policy, sending exactly that', async () => {
    const w = await open()
    await button(w, 'New').trigger('click')
    expect(button(w, 'Create').attributes('disabled')).toBeDefined() // no name yet
    await nameBox(w).setValue('  Scratch  ')
    await field(w, 'Working folder').setValue('/tmp/scratch')
    await button(w, 'Add a folder').trigger('click')
    await field(w, 'Folder 1').setValue('/tmp/more')
    await button(w, 'Add a variable').trigger('click')
    await field(w, 'Variable 1 name').setValue('TOKEN')
    await field(w, 'Variable 1 value').setValue('abc')
    await policyOf(w, 'Write').setValue('auto')
    await button(w, 'Create').trigger('click')
    await flushPromises()
    expect(sent('misty.env.save')).toEqual([
      { name: 'Scratch', description: '', cwd: '/tmp/scratch', roots: ['/tmp/more'], vars: { TOKEN: 'abc' }, policy: { write: 'auto' } },
    ])
    expect(w.text()).toContain('Saved "Scratch"')
    // it is in the list and open for changes
    expect(w.find('.list').text()).toContain('Scratch')
    expect(w.find('h3:not(.grow)').text()).toBe('Change this environment')
  })

  it('changes one by id, sends what was cleared as cleared, and leaves empty rows out', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    await field(w, 'Working folder').setValue('')
    await w.find('button[aria-label="Remove variable 1"]').trigger('click')
    await w.find('button[aria-label="Remove folder 1"]').trigger('click')
    await button(w, 'Add a variable').trigger('click') // left empty
    await policyOf(w, 'Commands').setValue('')
    await button(w, 'Save changes').trigger('click')
    await flushPromises()
    expect(sent('misty.env.save')).toEqual([
      { id: 'e1', name: 'My app', description: 'the app', cwd: null, roots: [], vars: {}, policy: {} },
    ])
  })

  it('asks before commands or the network run without asking, and only once', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    await policyOf(w, 'Commands').setValue('auto')
    expect(w.find('.error-box').text()).toContain('run commands in the working folder will run without asking')
    await button(w, 'Save changes').trigger('click')
    await flushPromises()
    expect(sent('misty.env.save')).toHaveLength(0)
    expect(w.find('.confirm-stub').text()).toContain('Allow this without asking?')
    await w.find('.confirm-stub .no').trigger('click')
    expect(sent('misty.env.save')).toHaveLength(0)
    await button(w, 'Save changes').trigger('click')
    await w.find('.confirm-stub .yes').trigger('click')
    await flushPromises()
    expect(sent('misty.env.save')).toHaveLength(1)
    expect(sent('misty.env.save')[0].policy).toEqual({ exec: 'auto' })
    // already allowed: changing something else does not ask again
    await nameBox(w).setValue('My app 2')
    await button(w, 'Save changes').trigger('click')
    await flushPromises()
    expect(w.find('.confirm-stub').exists()).toBe(false)
    expect(sent('misty.env.save')).toHaveLength(2)
  })

  it('shows what the hub refused and keeps the draft', async () => {
    const w = await open()
    await button(w, 'New').trigger('click')
    await nameBox(w).setValue('Bad')
    await field(w, 'Working folder').setValue('/nowhere')
    saveError = 'working folder: /nowhere: there is no such folder'
    await button(w, 'Create').trigger('click')
    await flushPromises()
    expect(w.find('.error-box').text()).toContain('no such folder')
    expect((field(w, 'Working folder').element as HTMLInputElement).value).toBe('/nowhere')
    expect(w.text()).toContain('unsaved changes')
  })

  it('duplicates into an unsaved copy', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    await button(w, 'Duplicate').trigger('click')
    expect((nameBox(w).element as HTMLInputElement).value).toBe('My app copy')
    expect(w.find('h3:not(.grow)').text()).toBe('New environment')
    await button(w, 'Create').trigger('click')
    await flushPromises()
    const made = sent('misty.env.save')[0]
    expect(made.id).toBeUndefined()
    expect(made).toMatchObject({ name: 'My app copy', cwd: '/work/app', roots: ['/work/data'], vars: { MODE: 'dev' } })
  })

  it('deletes after a question, and does not touch the hub when the answer is no', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    await button(w, 'Delete').trigger('click')
    expect(w.find('.confirm-stub').text()).toContain('Delete this environment?')
    await w.find('.confirm-stub .no').trigger('click')
    expect(sent('misty.env.delete')).toHaveLength(0)
    await button(w, 'Delete').trigger('click')
    await w.find('.confirm-stub .yes').trigger('click')
    await flushPromises()
    expect(sent('misty.env.delete')).toEqual([{ id: 'e1' }])
    expect(w.find('.list').text()).not.toContain('My app')
    expect(w.text()).toContain('Deleted "My app"')
  })

  it('asks before leaving changes that are not saved', async () => {
    envs.push({ ...structuredClone(APP), id: 'e2', name: 'Other' })
    const w = await open()
    await w.findAll('.list .link')[0].trigger('click')
    await nameBox(w).setValue('My app, edited')
    await w.findAll('.list .link')[1].trigger('click')
    expect(w.find('.confirm-stub').text()).toContain('Discard your changes?')
    expect((nameBox(w).element as HTMLInputElement).value).toBe('My app, edited')
    await w.find('.confirm-stub .yes').trigger('click')
    expect((nameBox(w).element as HTMLInputElement).value).toBe('Other')
    // with nothing changed there is no question
    await w.findAll('.list .link')[0].trigger('click')
    expect(w.find('.confirm-stub').exists()).toBe(false)
    expect((nameBox(w).element as HTMLInputElement).value).toBe('My app')
  })
})

describe('folders and using an environment', () => {
  it('fills the working folder and the other folders from the picker', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    const picker = w.findComponent(FolderPicker)
    expect(picker.props('open')).toBe(false)
    await button(w, 'Browse...').trigger('click') // the working folder's
    expect(picker.props('open')).toBe(true)
    expect(picker.props('start')).toBe('/work/app')
    picker.vm.$emit('pick', '/work/picked')
    await flushPromises()
    expect((field(w, 'Working folder').element as HTMLInputElement).value).toBe('/work/picked')
    expect(picker.props('open')).toBe(false)
    await w.findAll('button').filter((b) => b.text() === 'Browse...')[1].trigger('click') // the other folder's
    expect(picker.props('start')).toBe('/work/data')
    picker.vm.$emit('pick', '/work/other')
    await flushPromises()
    expect((field(w, 'Folder 1').element as HTMLInputElement).value).toBe('/work/other')
  })

  it('makes an environment the one a new Misty chat starts in', async () => {
    const w = await open()
    await w.find('.list .link').trigger('click')
    await button(w, 'Use in a new Misty chat').trigger('click')
    expect(localStorage.getItem('mm_misty_env')).toBe('e1')
    expect(push).toHaveBeenCalledWith('/misty')
  })
})

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'

vi.mock('@/transport', async (orig) => ({ ...(await orig<typeof import('@/transport')>()), call: vi.fn() }))

import { call } from '@/transport'
import FolderPicker from './FolderPicker.vue'

const PLACES = [{ label: 'Home', path: '/home/me' }, { label: '/', path: '/' }]
const TREE: Record<string, { parent: string | null; dirs: string[] }> = {
  '/home/me': { parent: '/home', dirs: ['app', 'docs', '.config'] },
  '/home/me/app': { parent: '/home/me', dirs: ['src'] },
  '/home/me/app/src': { parent: '/home/me/app', dirs: [] },
  '/': { parent: null, dirs: ['home'] },
}
const sent = () => vi.mocked(call).mock.calls.filter((c) => c[0] === 'misty.browse').map((c) => c[1] as any)

beforeAll(() => {
  // jsdom has no modal dialogs: just enough of them for the picker
  const proto = HTMLDialogElement.prototype as any
  proto.showModal = function () {
    this.setAttribute('open', '')
  }
  proto.close = function (value?: string) {
    if (!this.hasAttribute('open')) return
    this.removeAttribute('open')
    this.returnValue = value ?? ''
    this.dispatchEvent(new Event('close'))
  }
})

beforeEach(() => {
  vi.mocked(call).mockReset()
  vi.mocked(call).mockImplementation(async (op: string, args: any) => {
    if (op !== 'misty.browse') throw new Error(`no operation ${op}`)
    if (!args.path) return { path: null, parent: null, dirs: [], places: PLACES, truncated: false }
    const t = TREE[args.path]
    if (!t) throw new Error(`${args.path}: there is no such folder`)
    const dirs = t.dirs.filter((d) => args.hidden || !d.startsWith('.')).map((name) => ({ name, path: `${args.path === '/' ? '' : args.path}/${name}` }))
    return { path: args.path, parent: t.parent, dirs, places: PLACES, truncated: false }
  })
})

let wrapper: VueWrapper | undefined
afterEach(() => {
  wrapper?.unmount()
  wrapper = undefined
})

async function show(start: string | null = null) {
  wrapper = mount(FolderPicker, { props: { open: false, start }, attachTo: document.body })
  await wrapper.setProps({ open: true })
  await flushPromises()
  return wrapper
}
const names = (w: VueWrapper) => w.findAll('.folders .dir').map((b) => b.text())
const btn = (w: VueWrapper, text: string) => w.findAll('button').find((b) => b.text() === text)!

describe('choosing a folder', () => {
  it('opens at the starting places when there is no start, and goes into a folder', async () => {
    const w = await show()
    expect(sent()[0]).toEqual({})
    expect(w.findAll('.places button').map((b) => b.text())).toEqual(['Home', '/'])
    expect(w.text()).toContain('Choose a place above')
    expect(btn(w, 'Use this folder').attributes('disabled')).toBeDefined()
    await btn(w, 'Home').trigger('click')
    await flushPromises()
    expect(names(w)).toEqual(['.. (up one folder)', 'app', 'docs']) // hidden folders are not listed
    await btn(w, 'app').trigger('click')
    await flushPromises()
    expect((w.find('input[type="text"]').element as HTMLInputElement).value).toBe('/home/me/app')
    expect(w.find('.cur').text()).toBe('/home/me/app')
  })

  it('opens at the folder it is given, goes up, and lists hidden folders when asked', async () => {
    const w = await show('/home/me/app')
    expect(sent()[0]).toEqual({ path: '/home/me/app' })
    expect(names(w)).toEqual(['.. (up one folder)', 'src'])
    await btn(w, '.. (up one folder)').trigger('click')
    await flushPromises()
    expect(w.find('.cur').text()).toBe('/home/me')
    await w.find('input[type="checkbox"]').setValue(true)
    await flushPromises()
    expect(names(w)).toContain('.config')
    expect(sent().pop()).toEqual({ path: '/home/me', hidden: true })
  })

  it('goes to a typed path, and a path that is not there says so and keeps the place', async () => {
    const w = await show('/home/me')
    await w.find('input[type="text"]').setValue('  "/home/me/app/src"  ')
    await w.find('form').trigger('submit')
    await flushPromises()
    expect(w.find('.cur').text()).toBe('/home/me/app/src')
    expect(w.text()).toContain('No folders in here')
    await w.find('input[type="text"]').setValue('/nowhere')
    await w.find('form').trigger('submit')
    await flushPromises()
    expect(w.find('.error-box').text()).toContain('no such folder')
    expect(w.find('.cur').text()).toBe('/home/me/app/src')
  })

  it('shows the starting places when the folder it was given is gone', async () => {
    const w = await show('/gone')
    expect(w.findAll('.places button')).toHaveLength(2)
    expect(w.find('.cur').text()).toBe('no folder chosen yet')
  })

  it('emits the folder it is showing when it is used, and nothing when it is cancelled', async () => {
    const w = await show('/home/me/app')
    await btn(w, 'src').trigger('click')
    await flushPromises()
    await btn(w, 'Use this folder').trigger('click')
    expect(w.emitted('pick')).toEqual([['/home/me/app/src']])
    expect(w.emitted('cancel')).toBeUndefined()
    await w.setProps({ open: false })
    await w.setProps({ open: true })
    await flushPromises()
    await btn(w, 'Cancel').trigger('click')
    expect(w.emitted('pick')).toHaveLength(1)
    expect(w.emitted('cancel')).toHaveLength(1)
  })

  it('answers from the button even when the dialog never reports that it closed', async () => {
    // a page that is not being drawn (a background tab) never gets the dialog's close event
    const proto = HTMLDialogElement.prototype as any
    const real = proto.close
    proto.close = function (value?: string) {
      this.removeAttribute('open')
      this.returnValue = value ?? ''
    }
    try {
      const w = await show('/home/me/app')
      await btn(w, 'Use this folder').trigger('click')
      expect(w.emitted('pick')).toEqual([['/home/me/app']])
      await w.setProps({ open: false })
      await w.setProps({ open: true })
      await flushPromises()
      await btn(w, 'Cancel').trigger('click')
      expect(w.emitted('pick')).toHaveLength(1)
      expect(w.emitted('cancel')).toHaveLength(1)
    } finally {
      proto.close = real
    }
  })

  it('answers once for Escape, which sends cancel and then close', async () => {
    const w = await show('/home/me/app')
    const dlg = w.find('dialog').element as HTMLDialogElement
    dlg.dispatchEvent(new Event('cancel'))
    dlg.close('')
    expect(w.emitted('cancel')).toHaveLength(1)
    expect(w.emitted('pick')).toBeUndefined()
  })
})

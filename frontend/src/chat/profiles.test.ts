import { afterEach, describe, expect, it, vi } from 'vitest'
import { setToken } from '@/transport'
import { describe as words, isCustom, readProfiles, saveProfile, toForm, toProfile } from './profiles'

const ok = (result: unknown) =>
  new Response(JSON.stringify({ result }), { status: 200, headers: { 'Content-Type': 'application/json' } })
const refused = (message: string) =>
  new Response(JSON.stringify({ error: { message } }), { status: 400, headers: { 'Content-Type': 'application/json' } })

afterEach(() => {
  vi.unstubAllGlobals()
  setToken('')
})

describe('toForm and toProfile', () => {
  it('turns a profile into form values and back', () => {
    const p = { engine: 'prism', gpu_layers: 0, batch_size: 2048, cache_prompt: false, flash_attn: 'on' }
    const f = toForm(p)
    expect(f.engine).toBe('prism')
    expect(f.gpu_layers).toBe('0') // zero is a value, not "blank"
    expect(f.cache_prompt).toBe('false')
    expect(f.ctx_size).toBe('')
    expect(toProfile(f)).toEqual(p)
  })

  it('leaves blank fields out so the global setting applies', () => {
    expect(toProfile(toForm(undefined))).toEqual({})
    expect(toProfile({ gpu_layers: '  ', engine: '' })).toEqual({})
  })

  it('keeps what the form does not show and drops what is not a number', () => {
    const kept = toProfile({ ctx_size: '4096.7', parallel: 'many' }, { cache_reuse: 256, ctx_size: 1 })
    expect(kept).toEqual({ cache_reuse: 256, ctx_size: 4096 })
  })

  it('accepts a number a number input handed back', () => {
    expect(toProfile({ gpu_layers: 999 as unknown as string })).toEqual({ gpu_layers: 999 })
  })
})

describe('describe and isCustom', () => {
  it('says nothing for a profile with no settings', () => {
    expect(isCustom(undefined)).toBe(false)
    expect(isCustom({})).toBe(false)
    expect(words({})).toBe('')
  })

  it('puts the main settings in words', () => {
    expect(words({ engine: 'prism', gpu_layers: 0, batch_size: 2048 })).toBe('engine prism, CPU only, batch 2048')
    expect(words({ gpu_layers: 999, ctx_size: 8192, parallel: 2 })).toBe('999 GPU layers, context 8192, 2 slots')
    expect(words({ jinja: true })).toBe('1 setting')
  })
})

describe('saving', () => {
  const hub = (cfg: Record<string, unknown>, set: () => Response = () => ok(cfg)) => {
    const calls: { op: string; body: any }[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init: RequestInit) => {
        const op = String(url).split('/').pop() as string
        calls.push({ op, body: JSON.parse(String(init.body)) })
        return op === 'config.set' ? set() : ok(cfg)
      }),
    )
    setToken('secret')
    return calls
  }

  it('reads the profiles and the hub warnings', async () => {
    hub({ profiles: { a: { gpu_layers: 0 } }, config_warnings: ['profiles."b".ctx_size: ignored'] })
    const r = await readProfiles()
    expect(r.profiles).toEqual({ a: { gpu_layers: 0 } })
    expect(r.warnings).toHaveLength(1)
  })

  it('writes the whole table back with only the one profile changed', async () => {
    const calls = hub({ profiles: { a: { gpu_layers: 0 } } })
    await saveProfile('b', { engine: 'prism' })
    const set = calls.find((c) => c.op === 'config.set')!
    expect(set.body).toEqual({ settings: { profiles: { a: { gpu_layers: 0 }, b: { engine: 'prism' } } } })
  })

  it('removes the profile when it is saved empty', async () => {
    const calls = hub({ profiles: { a: { gpu_layers: 0 }, b: { engine: 'prism' } } })
    await saveProfile('a', {})
    expect(calls.find((c) => c.op === 'config.set')!.body).toEqual({ settings: { profiles: { b: { engine: 'prism' } } } })
  })

  it('rejects with the reason the hub gives', async () => {
    hub({ profiles: {} }, () => refused('profiles."a".cache_type_k: unknown type zz'))
    await expect(saveProfile('a', { cache_type_k: 'zz' })).rejects.toMatchObject({ message: expect.stringContaining('unknown type zz') })
  })
})

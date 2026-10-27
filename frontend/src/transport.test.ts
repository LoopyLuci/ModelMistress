import { afterEach, describe, expect, it, vi } from 'vitest'
import { auth, call, setToken } from './transport'

const reply = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })

afterEach(() => {
  vi.unstubAllGlobals()
  setToken('')
})

describe('call', () => {
  it('posts the operation with the bearer token and returns its result', async () => {
    const fetchMock = vi.fn(async () => reply(200, { result: [{ id: 'm' }] }))
    vi.stubGlobal('fetch', fetchMock)
    setToken('secret')

    const result = await call('model.list', { refresh: true })

    expect(result).toEqual([{ id: 'm' }])
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(url).toBe('/v1/call/model.list')
    expect(init.method).toBe('POST')
    expect(new Headers(init.headers).get('Authorization')).toBe('Bearer secret')
    expect(init.body).toBe(JSON.stringify({ refresh: true }))
  })

  it('rejects with the hub error message and status', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => reply(500, { error: { message: 'boom' } })))
    setToken('secret')

    await expect(call('model.load', { model: 'x' })).rejects.toMatchObject({ message: 'boom', status: 500 })
  })

  it('signs out when the hub refuses the token', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => reply(401, { error: { message: 'no' } })))
    setToken('stale')

    await expect(call('service.status')).rejects.toThrow('not signed in')
    expect(auth.token).toBe('')
    expect(auth.error).toBe('That token was not accepted.')
  })
})

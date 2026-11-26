import { describe, expect, it, vi } from 'vitest'

const hubFetch = vi.hoisted(() => vi.fn())
vi.mock('@/chat/hub', async (orig) => ({ ...(await orig<typeof import('@/chat/hub')>()), hubFetch }))

import { importModel } from './modelImport'

describe('importing a model file', () => {
  it('sends the file as its body, with its name percent-encoded in a header', async () => {
    hubFetch.mockResolvedValueOnce({ ok: true, json: async () => ({ name: 'Été 1.gguf', bytes: 9, path: '/h/models/Été 1.gguf' }) })
    const f = new File(['GGUFxxxxx'], 'Été 1.gguf', { type: 'application/octet-stream' })
    const got = await importModel(f)
    expect(got.name).toBe('Été 1.gguf')
    const [path, init] = hubFetch.mock.calls[0]
    expect(path).toBe('/v1/models/import')
    expect(init.method).toBe('POST')
    expect(init.headers['x-file-name']).toBe(encodeURIComponent('Été 1.gguf'))
    expect(init.body).toBe(f)
  })

  it('says why the hub refused a file', async () => {
    const body = { error: { message: 'not a GGUF model file' } }
    const refused = { ok: false, status: 400, json: async () => body, text: async () => JSON.stringify(body) }
    // failure() reads a copy of the body, as a fetch response allows
    hubFetch.mockResolvedValueOnce({ ...refused, clone: () => ({ json: async () => body }) })
    await expect(importModel(new File(['x'], 'a.gguf'))).rejects.toThrow('not a GGUF model file')
  })
})

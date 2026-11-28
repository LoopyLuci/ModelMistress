import { describe, expect, it } from 'vitest'
import { canSendTo, newDevice, type DeviceSetting } from './devices'

describe('devices', () => {
  it('names a new device phone-1, then the first free number', () => {
    expect(newDevice([])).toEqual({ name: 'phone-1', url: '', key: '' })
    const taken: DeviceSetting[] = [
      { name: 'phone-1', url: 'http://a', key: '(set)' },
      { name: 'phone-2', url: 'http://b', key: '(set)' },
    ]
    expect(newDevice(taken).name).toBe('phone-3')
  })

  it('offers a model to a device unless it is an Ollama model', () => {
    expect(canSendTo({ source: 'huggingface' })).toBe(true)
    expect(canSendTo({ source: 'native' })).toBe(true)
    expect(canSendTo({ source: 'folder' })).toBe(true)
    expect(canSendTo({ source: 'ollama' })).toBe(false)
    expect(canSendTo({ source: 'ollama-daemon' })).toBe(false)
  })
})

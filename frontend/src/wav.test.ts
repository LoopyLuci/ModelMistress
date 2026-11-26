import { describe, expect, it } from 'vitest'
import { wavBytes } from './wav'

describe('wavBytes', () => {
  it('writes a 16-bit mono PCM WAV that the hub reads, with the samples clamped to the range', () => {
    const bytes = new DataView(wavBytes(new Float32Array([0, 1, -1, 2]), 16000))
    const text = (at: number) => String.fromCharCode(...[0, 1, 2, 3].map((i) => bytes.getUint8(at + i)))
    expect(text(0)).toBe('RIFF')
    expect(text(8)).toBe('WAVE')
    expect(text(12)).toBe('fmt ')
    expect(text(36)).toBe('data')
    expect(bytes.byteLength).toBe(44 + 4 * 2)
    expect(bytes.getUint32(4, true)).toBe(36 + 8)
    expect(bytes.getUint16(20, true)).toBe(1) // PCM
    expect(bytes.getUint16(22, true)).toBe(1) // mono
    expect(bytes.getUint32(24, true)).toBe(16000)
    expect(bytes.getUint32(28, true)).toBe(32000) // bytes per second
    expect(bytes.getUint16(34, true)).toBe(16)
    expect(bytes.getUint32(40, true)).toBe(8)
    expect(bytes.getInt16(44, true)).toBe(0)
    expect(bytes.getInt16(46, true)).toBe(32767)
    expect(bytes.getInt16(48, true)).toBe(-32768)
    expect(bytes.getInt16(50, true)).toBe(32767) // 2 is clamped to 1
  })
})

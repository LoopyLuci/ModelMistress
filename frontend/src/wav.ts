// Recordings to WAV for the hub's speech to text. The hub reads WAV (and MP3 when built with whisper); a browser's
// microphone recording is WebM or Ogg, which it refuses, so it is decoded here and written out as 16-bit PCM first.

/** The bytes of a 16-bit mono PCM WAV file of `samples` (each in [-1, 1]) at `rate` samples per second. */
export function wavBytes(samples: Float32Array, rate: number): ArrayBuffer {
  const n = samples.length
  const buf = new ArrayBuffer(44 + n * 2)
  const v = new DataView(buf)
  const text = (at: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(at + i, s.charCodeAt(i))
  }
  text(0, 'RIFF')
  v.setUint32(4, 36 + n * 2, true)
  text(8, 'WAVE')
  text(12, 'fmt ')
  v.setUint32(16, 16, true) // the PCM header is 16 bytes
  v.setUint16(20, 1, true) // PCM
  v.setUint16(22, 1, true) // mono
  v.setUint32(24, rate, true)
  v.setUint32(28, rate * 2, true) // bytes per second
  v.setUint16(32, 2, true) // bytes per frame
  v.setUint16(34, 16, true) // bits per sample
  text(36, 'data')
  v.setUint32(40, n * 2, true)
  for (let i = 0; i < n; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]))
    v.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true)
  }
  return buf
}

/** Any recording the browser can decode (WebM, Ogg, MP4, MP3, WAV) as a mono WAV file. */
export async function toWav(blob: Blob): Promise<Blob> {
  const ctx = new AudioContext()
  try {
    const decoded = await ctx.decodeAudioData(await blob.arrayBuffer())
    const mono = new Float32Array(decoded.length)
    const channels = decoded.numberOfChannels
    for (let c = 0; c < channels; c++) {
      const data = decoded.getChannelData(c)
      for (let i = 0; i < data.length; i++) mono[i] += data[i] / channels
    }
    return new Blob([wavBytes(mono, decoded.sampleRate)], { type: 'audio/wav' })
  } catch (e: any) {
    throw new Error(`this audio could not be read: ${e?.message || e}`)
  } finally {
    ctx.close().catch(() => {})
  }
}

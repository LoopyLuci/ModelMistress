// The hub's speech for the chat: dictation (speech to text) and read-aloud (text to speech). Both use the hub's
// OpenAI-style audio routes (src/audio.rs, src/tts.rs), the same ones the Speech page uses.
import { failure, hubFetch } from '@/chat/hub'
import { toWav } from '@/wav'

/** Starts recording from the microphone. `stop()` ends it and gives the recording as one clip. */
export async function startRecording(): Promise<{ stop: () => Promise<Blob> }> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
  const rec = new MediaRecorder(stream)
  const chunks: Blob[] = []
  rec.ondataavailable = (e) => chunks.push(e.data)
  const ended = new Promise<Blob>((ok) => {
    rec.onstop = () => {
      stream.getTracks().forEach((t) => t.stop())
      ok(new Blob(chunks, { type: rec.mimeType || 'audio/webm' }))
    }
  })
  rec.start()
  return {
    stop: () => {
      if (rec.state !== 'inactive') rec.stop()
      return ended
    },
  }
}

/** The words in a recording, by the hub's speech to text. The clip is written out as WAV first (the hub reads no WebM). */
export async function transcribeClip(clip: Blob): Promise<string> {
  const fd = new FormData()
  fd.append('file', await toWav(clip), 'speech.wav')
  const r = await hubFetch('/v1/audio/transcriptions', { method: 'POST', body: fd })
  if (!r.ok) throw await failure(r)
  return ((await r.json()).text || '').trim()
}

/** `text` spoken by the hub's text to speech, as a WAV clip. */
export async function speakClip(text: string): Promise<Blob> {
  const r = await hubFetch('/v1/audio/speech', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'tts-1', input: text, response_format: 'wav' }),
  })
  if (!r.ok) throw await failure(r)
  return r.blob()
}

/** True when the error says this build does not have the feature (the hub answers 501, or 404 for a route it lacks). */
export const notInBuild = (e: any): boolean => e?.status === 501 || e?.status === 404

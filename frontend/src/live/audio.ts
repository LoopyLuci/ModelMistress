// The browser side of Live: the microphone (its level every tenth of a second, and each sentence the owner finishes as a
// recording) and the voice that says the reply aloud (the hub's text to speech, sentence by sentence). The rules of the
// conversation are in turns.ts; this file only moves sound, and reports back through the callbacks it is given.

import { speakClip } from '@/speech'
import { DEFAULT_SEGMENTER, Segmenter } from './segmenter'

const FRAME_MS = 100

export interface MicHandlers {
  /** One sentence the owner finished, as a recording, with how long it was. */
  onUtterance: (clip: Blob, durationMs: number) => void
}

export class LiveMic {
  private stream: MediaStream | null = null
  private ctx: AudioContext | null = null
  private timer: number | undefined
  private readonly seg = new Segmenter(DEFAULT_SEGMENTER)
  private recorder: MediaRecorder | null = null
  private chunks: Blob[] = []
  private enabled = false

  constructor(private readonly handlers: MicHandlers) {}

  /** Asks for the microphone and starts reading its level. Rejects when the owner does not allow it or there is none. */
  async open(): Promise<void> {
    // The browser removes the phone's own voice from the microphone where it can (echo cancellation), so less of the reply
    // comes back as the owner's talk.
    const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } })
    this.stream = stream
    const ctx = new AudioContext()
    const analyser = ctx.createAnalyser()
    analyser.fftSize = 2048
    ctx.createMediaStreamSource(stream).connect(analyser)
    this.ctx = ctx
    const buf = new Float32Array(analyser.fftSize)
    this.timer = window.setInterval(() => {
      analyser.getFloatTimeDomainData(buf)
      let sum = 0
      for (const x of buf) sum += x * x
      this.frame(Math.sqrt(sum / buf.length), performance.now())
    }, FRAME_MS)
  }

  /** On: the microphone listens. Off: an utterance in progress is dropped, and nothing is heard until it is on again. */
  setEnabled(on: boolean): void {
    if (on === this.enabled) return
    this.enabled = on
    if (!on) {
      this.seg.reset()
      this.finish(false)
    }
  }

  /** Stops the microphone for good. */
  close(): void {
    window.clearInterval(this.timer)
    this.enabled = false
    if (this.recorder) {
      this.recorder.onstop = null
      if (this.recorder.state !== 'inactive') this.recorder.stop()
    }
    this.recorder = null
    this.stream?.getTracks().forEach((t) => t.stop())
    void this.ctx?.close()
    this.stream = null
    this.ctx = null
  }

  private frame(level: number, now: number): void {
    if (!this.enabled) return
    const e = this.seg.push(level, now)
    if (!e) return
    if (e.kind === 'start') this.begin()
    else this.finish(e.keep, e.durationMs)
  }

  private begin(): void {
    if (!this.stream || this.recorder) return
    const rec = new MediaRecorder(this.stream)
    this.chunks = []
    rec.ondataavailable = (e) => {
      if (e.data.size > 0) this.chunks.push(e.data)
    }
    rec.start()
    this.recorder = rec
  }

  private finish(keep: boolean, durationMs = 0): void {
    const rec = this.recorder
    this.recorder = null
    if (!rec) return
    const chunks = this.chunks
    this.chunks = []
    rec.onstop = () => {
      if (keep) this.handlers.onUtterance(new Blob(chunks, { type: rec.mimeType || 'audio/webm' }), durationMs)
    }
    if (rec.state !== 'inactive') rec.stop()
  }
}

export class LiveVoice {
  private queue: { text: string; done: (failure?: unknown) => void }[] = []
  private audio: HTMLAudioElement | null = null
  private url: string | null = null
  /** Bumped on stop, so that a clip still being made is dropped. */
  private gen = 0

  /** Says `sentence` after what is queued. `done` runs once it has been said, or when it could not be (with why). */
  say(sentence: string, done: (failure?: unknown) => void): void {
    this.queue.push({ text: sentence, done })
    if (this.queue.length === 1) void this.next()
  }

  /** Stops the speech and drops what is queued. No `done` runs for what is dropped. */
  stop(): void {
    this.gen++
    this.queue = []
    this.audio?.pause()
    this.audio = null
    this.release()
  }

  private release(): void {
    if (this.url) URL.revokeObjectURL(this.url)
    this.url = null
  }

  private async next(): Promise<void> {
    const item = this.queue[0]
    if (!item) return
    const gen = this.gen
    let finished = false
    const finish = (failure?: unknown) => {
      if (finished || gen !== this.gen) return
      finished = true
      this.release()
      this.queue.shift()
      item.done(failure)
      void this.next()
    }
    try {
      const clip = await speakClip(item.text)
      if (gen !== this.gen) return
      this.url = URL.createObjectURL(clip)
      const a = new Audio(this.url)
      this.audio = a
      a.onended = () => finish()
      a.onerror = (e) => finish(e)
      await a.play()
    } catch (e) {
      finish(e)
    }
  }
}

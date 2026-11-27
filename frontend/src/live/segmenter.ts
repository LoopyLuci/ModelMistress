// Finds where the owner's sentences start and end in the sound from the microphone, from the level of each short frame of it
// (voice activity detection). Pure: the browser glue feeds it the level of a frame every tenth of a second, and it says when
// an utterance begins and when it has ended.

export interface SegmenterOptions {
  /** The level (0 to 1, the RMS of a frame) at or above which a frame is speech. */
  threshold: number
  /** How long the quiet must last to end an utterance. */
  silenceMs: number
  /** An utterance shorter than this is noise (a click, a cough) and is dropped. */
  minMs: number
  /** An utterance is ended at this length, however long the owner talks. */
  maxMs: number
}

export const DEFAULT_SEGMENTER: SegmenterOptions = { threshold: 0.02, silenceMs: 700, minMs: 300, maxMs: 20000 }

export type SegmentEvent =
  | { kind: 'start' }
  /** `durationMs` is from the first speech frame to the last one. `keep` is false for noise. */
  | { kind: 'end'; durationMs: number; keep: boolean }

export class Segmenter {
  private speaking = false
  private startedAt = 0
  private lastVoice = 0

  constructor(private readonly o: SegmenterOptions = DEFAULT_SEGMENTER) {}

  /** Feed the level of one frame, with the time it was taken (milliseconds). Returns an event when one begins or ends. */
  push(level: number, now: number): SegmentEvent | null {
    if (!this.speaking) {
      if (level < this.o.threshold) return null
      this.speaking = true
      this.startedAt = now
      this.lastVoice = now
      return { kind: 'start' }
    }
    if (level >= this.o.threshold) this.lastVoice = now
    const quiet = now - this.lastVoice
    const length = now - this.startedAt
    if (quiet < this.o.silenceMs && length < this.o.maxMs) return null
    return this.finish()
  }

  /** Ends the utterance in progress, if there is one (the microphone was turned off, or the owner ended the conversation). */
  reset(): SegmentEvent | null {
    return this.speaking ? this.finish() : null
  }

  private finish(): SegmentEvent {
    this.speaking = false
    const durationMs = this.lastVoice - this.startedAt
    return { kind: 'end', durationMs, keep: durationMs >= this.o.minMs }
  }
}

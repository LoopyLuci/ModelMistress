import { describe, expect, it } from 'vitest'
import { Segmenter, type SegmenterOptions } from './segmenter'

const OPTS: SegmenterOptions = { threshold: 0.1, silenceMs: 500, minMs: 300, maxMs: 2000 }

/** Feeds a frame every 100 ms: `levels` is one level per frame. Returns the events in order. */
function run(levels: number[], opts = OPTS) {
  const s = new Segmenter(opts)
  const events: unknown[] = []
  levels.forEach((level, i) => {
    const e = s.push(level, i * 100)
    if (e) events.push({ at: i * 100, ...e })
  })
  return events
}

describe('the voice segmenter', () => {
  it('starts when a frame is loud enough and ends after a silence', () => {
    const levels = [0, 0, 0.5, 0.6, 0.4, 0, 0, 0, 0, 0, 0]
    expect(run(levels)).toEqual([
      { at: 200, kind: 'start' },
      // the last speech frame is at 400 ms; the quiet lasts 500 ms after it
      { at: 900, kind: 'end', durationMs: 200, keep: false },
    ])
  })

  it('keeps an utterance that lasts long enough', () => {
    const levels = [0.5, 0.5, 0.5, 0.5, 0, 0, 0, 0, 0, 0]
    const ev = run(levels) as any[]
    expect(ev[1]).toMatchObject({ kind: 'end', durationMs: 300, keep: true })
  })

  it('cuts an utterance at the longest length and starts the next one, however long the owner talks', () => {
    const levels = Array.from({ length: 30 }, () => 0.5)
    const ev = run(levels) as any[]
    expect(ev.map((e) => e.kind)).toEqual(['start', 'end', 'start'])
    expect(ev[1]).toMatchObject({ at: 2000, durationMs: 2000, keep: true })
  })

  it('reports an utterance cut off by reset, and nothing when silent', () => {
    const s = new Segmenter(OPTS)
    expect(s.reset()).toBeNull()
    s.push(0.5, 0)
    s.push(0.5, 100)
    expect(s.reset()).toEqual({ kind: 'end', durationMs: 100, keep: false })
  })
})

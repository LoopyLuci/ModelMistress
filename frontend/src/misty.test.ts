import { describe, expect, it } from 'vitest'
import { pairRuns, type RunRow, type Turn } from './misty'

const run = (id: string, input: string, status = 'done', session = 's'): RunRow => ({
  id, session, input, status, started: 0, ms: 0,
})

describe('pairRuns', () => {
  it('gives each reply the run that made it, matching the questions in order', () => {
    const turns: Turn[] = [
      { role: 'user', content: 'a' },
      { role: 'assistant', content: 'A' },
      { role: 'user', content: 'b' },
      { role: 'assistant', content: 'B' },
    ]
    // the run log is newest first
    const runs = [run('r2', 'b'), run('r1', 'a'), run('other', 'a', 'done', 't')]
    const out = pairRuns(turns, runs, 's')
    expect(out[1].run).toBe('r1')
    expect(out[3].run).toBe('r2')
    // the turns passed in are not changed
    expect(turns[1].run).toBeUndefined()
  })

  it('does not match a run that failed or was cancelled, so no reply is paired with it', () => {
    const turns: Turn[] = [
      { role: 'user', content: 'b' },
      { role: 'assistant', content: 'B' },
    ]
    // the failed run asked the same question first; the finished one is the reply shown
    const runs = [run('done1', 'b'), run('failed', 'b', 'error')]
    expect(pairRuns(turns, runs, 's')[1].run).toBe('done1')
    expect(pairRuns(turns, [run('failed', 'b', 'error')], 's')[1].run).toBeUndefined()
  })

  it('leaves a reply alone when no run matches it, and matches on the first 200 characters of a question', () => {
    const long = 'x'.repeat(300)
    const turns: Turn[] = [
      { role: 'user', content: long },
      { role: 'assistant', content: 'long answer' },
      { role: 'user', content: 'unknown' },
      { role: 'assistant', content: 'no run for this' },
    ]
    const out = pairRuns(turns, [run('r1', long.slice(0, 200))], 's')
    expect(out[1].run).toBe('r1')
    expect(out[3].run).toBeUndefined()
  })
})

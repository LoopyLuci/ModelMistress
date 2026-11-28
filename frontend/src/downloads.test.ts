import { describe, expect, it } from 'vitest'
import { canRestart, liveDownloads, type DownloadJob } from './downloads'

function job(over: Partial<DownloadJob>): DownloadJob {
  return {
    job: 'pull-1',
    kind: 'pull',
    model: 'org/M-GGUF:M-Q4_K_M',
    source: 'huggingface',
    status: 'running',
    message: 'file 1 of 2: M-Q4_K_M.gguf',
    percent: 40,
    mb_per_s: 12.5,
    done_bytes: 400,
    total_bytes: 1000,
    started: 1,
    ...over,
  }
}

describe('downloads', () => {
  it('shows running, stopped and failed downloads on every tab, but not finished ones or tasks', () => {
    const jobs = [
      job({ job: 'a', status: 'running' }),
      job({ job: 'b', status: 'cancelled' }),
      job({ job: 'c', status: 'failed' }),
      job({ job: 'd', status: 'done' }),
      job({ job: 'e', status: 'running', source: 'task' }),
    ]
    expect(liveDownloads(jobs).map((j) => j.job)).toEqual(['a', 'b', 'c'])
  })

  it('shows a model being sent to a phone alongside the downloads, but not other tasks', () => {
    const jobs = [
      job({ job: 'send-1', kind: 'send', source: 'task', status: 'running', model: 'Tiny.gguf to phone' }),
      job({ job: 'quant-1', kind: 'quantize', source: 'task', status: 'running' }),
      job({ job: 'send-2', kind: 'send', source: 'task', status: 'done' }),
    ]
    expect(liveDownloads(jobs).map((j) => j.job)).toEqual(['send-1'])
  })

  it('restarts a stopped or failed download, and nothing else', () => {
    expect(canRestart(job({ status: 'cancelled' }))).toBe(true)
    expect(canRestart(job({ status: 'failed' }))).toBe(true)
    expect(canRestart(job({ status: 'running' }))).toBe(false)
    expect(canRestart(job({ status: 'done' }))).toBe(false)
    expect(canRestart(job({ status: 'failed', source: 'task' }))).toBe(false)
    expect(canRestart(job({ kind: 'send', status: 'failed', source: 'task' }))).toBe(false)
  })
})

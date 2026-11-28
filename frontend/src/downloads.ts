/** A job as the hub's `job.list` reports it: a download, a model sent to a phone (`kind: 'send'`), or another task such as a quantize. */
export interface DownloadJob {
  job: string
  kind: string
  model: string
  source: string
  status: string
  message: string
  percent: number
  mb_per_s: number
  done_bytes: number
  total_bytes: number
  started: number
}

/** A transfer the Models screen shows: a download, or a model being sent to a phone. Other tasks are not transfers. */
export function isTransfer(j: DownloadJob): boolean {
  return j.source !== 'task' || j.kind === 'send'
}

/** The transfers a page shows on every tab: the running ones, and the stopped or failed ones that can start again. */
export function liveDownloads(jobs: DownloadJob[]): DownloadJob[] {
  return jobs.filter(
    (j) => isTransfer(j) && (j.status === 'running' || j.status === 'cancelled' || j.status === 'failed'),
  )
}

/** A stopped or failed download starts again from the part it has. A send or a task cannot, and a finished job has nothing to do. */
export function canRestart(j: DownloadJob): boolean {
  return j.source !== 'task' && (j.status === 'cancelled' || j.status === 'failed')
}

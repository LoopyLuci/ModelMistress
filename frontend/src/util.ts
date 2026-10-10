// Small formatting and text helpers shared by the Knowledge, Data, and Factory pages.

export function fmtBytes(n: number | null | undefined): string {
  if (n == null) return '-'
  if (n < 1024) return `${n} B`
  const u = ['KiB', 'MiB', 'GiB', 'TiB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(v < 10 ? 2 : 1)} ${u[i]}`
}

export function fmtWhen(ms: number | null | undefined): string {
  return ms ? new Date(ms).toLocaleString() : '-'
}

export function fmtNum(n: number | null | undefined, d = 3): string {
  return n == null || Number.isNaN(n) ? '-' : Number(n).toFixed(d)
}

/** A line-level diff (longest common subsequence) of two texts; ops are 'same' | 'del' | 'ins'. */
export function diffLines(a: string, b: string): { op: 'same' | 'del' | 'ins'; text: string }[] {
  const x = a.split('\n')
  const y = b.split('\n')
  const n = x.length
  const m = y.length
  const t: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0))
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) t[i][j] = x[i] === y[j] ? t[i + 1][j + 1] + 1 : Math.max(t[i + 1][j], t[i][j + 1])
  const out: { op: 'same' | 'del' | 'ins'; text: string }[] = []
  let i = 0
  let j = 0
  while (i < n && j < m) {
    if (x[i] === y[j]) out.push({ op: 'same', text: x[i++] }), j++
    else if (t[i + 1][j] >= t[i][j + 1]) out.push({ op: 'del', text: x[i++] })
    else out.push({ op: 'ins', text: y[j++] })
  }
  while (i < n) out.push({ op: 'del', text: x[i++] })
  while (j < m) out.push({ op: 'ins', text: y[j++] })
  return out
}

/** A record as text for diffing: pretty JSON without the revision bookkeeping. */
export function recordText(r: any): string {
  if (!r) return ''
  const { rev, edited_by, ...rest } = r
  return JSON.stringify(rest, null, 2)
}

export function splitList(s: string): string[] {
  return s
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean)
}

// Plain HTTP to the hub for the pages that need more than an operation call: streamed chat, audio, images.
// If the transport offers a fetch helper (the desktop app does) it is used; otherwise this is fetch with the owner
// token in a header, exactly as transport.ts does in a browser.
import * as transport from '@/transport'
import { auth, signOut } from '@/transport'

const base = (): string => ((import.meta as any).env?.VITE_HUB_URL as string) || ''

export async function hubFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const helper = (transport as any).hubFetch as ((p: string, i?: RequestInit) => Promise<Response>) | undefined
  if (helper) return helper(path, init)
  const headers = new Headers(init.headers || {})
  headers.set('Authorization', `Bearer ${auth.token}`)
  let r: Response
  try {
    r = await fetch(base() + path, { ...init, headers })
  } catch (e) {
    if ((e as any)?.name === 'AbortError') throw e
    throw new Error('the hub is not reachable')
  }
  if (r.status === 401 || r.status === 403) {
    signOut(r.status === 401 ? 'That token was not accepted.' : 'The hub refused this token.')
    throw new Error('not signed in')
  }
  return r
}

/** The error of a failed response: the hub's message, with the status on it (404/501: not in this build). */
export async function failure(r: Response): Promise<Error> {
  let msg: any = `${r.status} ${r.statusText}`
  try {
    const j = await r.clone().json()
    msg = j?.error?.message || j?.error || msg
  } catch (_) {
    /* not JSON */
  }
  const e: any = new Error(typeof msg === 'string' ? msg : JSON.stringify(msg))
  e.status = r.status
  return e
}

/** Read server-sent events: calls onData for every `data:` payload until the stream ends. */
export async function readSse(r: Response, onData: (data: string) => void): Promise<void> {
  const reader = r.body!.getReader()
  const dec = new TextDecoder()
  let buf = ''
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    buf += dec.decode(value, { stream: true })
    let i: number
    while ((i = buf.search(/\r?\n\r?\n/)) >= 0) {
      const block = buf.slice(0, i)
      buf = buf.slice(i).replace(/^\r?\n\r?\n/, '')
      const data = block
        .split(/\r?\n/)
        .filter((l) => l.startsWith('data:'))
        .map((l) => l.slice(5).replace(/^ /, ''))
        .join('\n')
      if (data) onData(data)
    }
  }
}

export interface ChatParams { temperature: number; top_p: number; max_tokens: number }

export interface ChatMessage {
  role: 'system' | 'user' | 'assistant'
  content: string
  reasoning?: string
  images?: string[]
}

export interface ReplyStats {
  ttft_ms: number | null
  total_ms: number
  prompt_tokens: number | null
  completion_tokens: number | null
  tps: number | null
}

export interface StreamHandlers {
  onContent: (delta: string) => void
  onReasoning: (delta: string) => void
  /** Called at the first token of any kind (the answer started). */
  onFirst?: () => void
}

/** POST /v1/chat/completions with stream:true; resolves with the stats when the stream ends (or rejects on error;
 *  an abort rejects with an AbortError, text so far is kept by the handlers). */
export async function streamChat(
  model: string,
  messages: ChatMessage[],
  p: ChatParams,
  trace: boolean,
  signal: AbortSignal,
  h: StreamHandlers,
): Promise<ReplyStats> {
  const wire = messages.map((m) => {
    if (m.role === 'user' && m.images?.length) {
      return {
        role: 'user',
        content: [
          ...(m.content ? [{ type: 'text', text: m.content }] : []),
          ...m.images.map((url) => ({ type: 'image_url', image_url: { url } })),
        ],
      }
    }
    return { role: m.role, content: m.content }
  })
  const body: Record<string, unknown> = { model, messages: wire, stream: true, stream_options: { include_usage: true } }
  if (p.temperature != null) body.temperature = p.temperature
  if (p.top_p != null) body.top_p = p.top_p
  if (p.max_tokens > 0) body.max_tokens = p.max_tokens
  if (trace) body.mm_trace = true
  const t0 = performance.now()
  const r = await hubFetch('/v1/chat/completions', {
    method: 'POST',
    signal,
    headers: { 'Content-Type': 'application/json', Accept: 'text/event-stream' },
    body: JSON.stringify(body),
  })
  if (!r.ok) throw await failure(r)
  let first: number | null = null
  let chunks = 0
  let usage: any = null
  let timings: any = null
  let failed: string | null = null
  const mark = () => {
    if (first == null) {
      first = performance.now()
      h.onFirst?.()
    }
    chunks++
  }
  await readSse(r, (data) => {
    if (data === '[DONE]') return
    let j: any
    try {
      j = JSON.parse(data)
    } catch (_) {
      return
    }
    if (j.error) {
      failed = j.error.message || JSON.stringify(j.error)
      return
    }
    if (j.usage) usage = j.usage
    if (j.timings) timings = j.timings
    const d = j.choices?.[0]?.delta
    if (!d) return
    const rc = d.reasoning_content ?? d.reasoning
    if (rc) {
      mark()
      h.onReasoning(rc)
    }
    if (d.content) {
      mark()
      h.onContent(d.content)
    }
  })
  if (failed) throw new Error(failed)
  const end = performance.now()
  const completion = usage?.completion_tokens ?? timings?.predicted_n ?? chunks
  const genMs = timings?.predicted_ms ?? (first != null ? end - first : null)
  return {
    ttft_ms: first != null ? Math.round(first - t0) : null,
    total_ms: Math.round(end - t0),
    prompt_tokens: usage?.prompt_tokens ?? timings?.prompt_n ?? null,
    completion_tokens: completion ?? null,
    tps: timings?.predicted_per_second
      ? Math.round(timings.predicted_per_second * 10) / 10
      : genMs && completion
        ? Math.round((completion / (genMs / 1000)) * 10) / 10
        : null,
  }
}

// How the dashboard talks to the hub. Inside the desktop app (Tauri) it goes through the app's commands; in a plain
// browser (the hub serves this page at /ui) it uses fetch with the owner's token, which the page asks for once and
// keeps in sessionStorage (this tab only), never in the URL.
import { reactive } from 'vue'

const KEY = 'mm_token'

export const auth = reactive({ token: sessionStorage.getItem(KEY) || '', error: '' })

export const inTauri = (): boolean => !!(window as any).__TAURI__

export const needsToken = (): boolean => !inTauri() && !auth.token

export function setToken(t: string) {
  auth.token = t.trim()
  auth.error = ''
  if (auth.token) sessionStorage.setItem(KEY, auth.token)
  else sessionStorage.removeItem(KEY)
}

export function signOut(message = '') {
  sessionStorage.removeItem(KEY)
  auth.token = ''
  auth.error = message
}

// Same origin when the hub serves the page; VITE_HUB_URL points a dev server at a hub.
const base = (): string => ((import.meta as any).env?.VITE_HUB_URL as string) || ''

async function http(path: string, init: RequestInit = {}): Promise<Response> {
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

async function failure(r: Response): Promise<Error> {
  let msg = `${r.status} ${r.statusText}`
  try {
    const j = await r.json()
    msg = j?.error?.message || msg
  } catch (_) {
    /* not JSON */
  }
  const e: any = new Error(msg)
  e.status = r.status // pages tell "this operation does not exist here" (404) from a real failure
  return e
}

/** Run one hub operation (the same names MCP and POST /v1/call/{op} use). */
export async function call(op: string, args: Record<string, unknown> = {}): Promise<any> {
  if (inTauri()) return (window as any).__TAURI__.tauri.invoke('hub_call', { op, args })
  const r = await http(`/v1/call/${op}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(args),
  })
  if (!r.ok) throw await failure(r)
  return (await r.json()).result
}

export type LiveState = 'connecting' | 'live' | 'down'

/** Live trace events: server-sent events in a browser (read with fetch so the token stays in a header), polling in
 *  the desktop app. Returns a function that stops it. */
export function subscribe(onEvent: (e: any) => void, onState: (s: LiveState) => void): () => void {
  return inTauri() ? poll(onEvent, onState) : sse(onEvent, onState)
}

function sse(onEvent: (e: any) => void, onState: (s: LiveState) => void): () => void {
  const stop = new AbortController()
  const run = async () => {
    let wait = 1000
    while (!stop.signal.aborted) {
      if (!auth.token) {
        onState('down')
        await new Promise((ok) => setTimeout(ok, 500))
        continue
      }
      onState('connecting')
      try {
        const r = await http('/v1/native/stream', { signal: stop.signal, headers: { Accept: 'text/event-stream' } })
        if (!r.ok || !r.body) throw await failure(r)
        onState('live')
        wait = 1000
        const reader = r.body.getReader()
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
              .map((l) => l.slice(5).trim())
              .join('\n')
            if (data) {
              try {
                onEvent(JSON.parse(data))
              } catch (_) {
                /* a broken line */
              }
            }
          }
        }
      } catch (_) {
        if (stop.signal.aborted) return
      }
      onState('down')
      await new Promise((ok) => setTimeout(ok, wait))
      wait = Math.min(wait * 2, 5000)
    }
  }
  run()
  return () => stop.abort()
}

function poll(onEvent: (e: any) => void, onState: (s: LiveState) => void): () => void {
  let alive = true
  const seen = new Map<string, number>() // running trace id -> tokens already sent on
  const tick = async () => {
    try {
      const l = await call('trace.list', { status: 'running', limit: 20 })
      onState('live')
      const running = new Set<string>()
      for (const t of l.items as any[]) {
        running.add(t.id)
        if (!seen.has(t.id)) {
          seen.set(t.id, 0)
          onEvent({ type: 'trace.start', trace: t.id, model: t.model, kind: t.kind, ts: t.started_ms })
        }
        const have = seen.get(t.id)!
        const r = await call('trace.tokens', { id: t.id, offset: have, limit: 2000 })
        if (r.tokens.length) {
          seen.set(t.id, have + r.tokens.length)
          onEvent({ type: 'tokens', trace: t.id, offset: have, tokens: r.tokens })
        }
      }
      for (const id of [...seen.keys()]) {
        if (!running.has(id)) {
          seen.delete(id)
          onEvent({ type: 'trace.finish', trace: id })
        }
      }
    } catch (_) {
      onState('down')
    }
    if (alive) setTimeout(tick, 800)
  }
  tick()
  return () => {
    alive = false
  }
}

/** Send a chat request that is traced; the answer itself is not used here (the Thoughts page shows it live).
 *  Browser only. */
export async function chatTraced(model: string, prompt: string, signal: AbortSignal): Promise<void> {
  if (inTauri()) throw new Error('sending a request from here needs a browser; any client can send one with mm_trace')
  const r = await http('/v1/chat/completions', {
    method: 'POST',
    signal,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model, stream: true, mm_trace: true, messages: [{ role: 'user', content: prompt }] }),
  })
  if (!r.ok) throw await failure(r)
  const reader = r.body!.getReader()
  while (!(await reader.read()).done) {
    /* read to the end so the request completes */
  }
}

/** Save every matching trace as a JSON-lines file (browser) or ask the hub to write it (desktop app). */
export async function exportTraces(filter: Record<string, unknown>): Promise<string> {
  if (inTauri()) {
    const r = await call('trace.export', filter)
    return `Wrote ${r.count} trace(s) to ${r.path}`
  }
  const qs = new URLSearchParams(
    Object.entries(filter)
      .filter(([, v]) => v !== '' && v != null)
      .map(([k, v]) => [k, String(v)]),
  )
  const r = await http(`/v1/native/export?${qs}`)
  if (!r.ok) throw await failure(r)
  const blob = await r.blob()
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = 'traces.jsonl'
  a.click()
  URL.revokeObjectURL(a.href)
  return 'Downloaded traces.jsonl'
}

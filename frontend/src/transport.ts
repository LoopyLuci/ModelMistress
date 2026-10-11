// How the dashboard talks to the hub. In a plain browser (the hub serves this page at /ui) it uses fetch with the
// owner's token, which the page asks for once and keeps in sessionStorage (this tab only), never in the URL. Inside
// the desktop app (Tauri) the app tells the page where its hub is and the hub's token (`hub_info`), and the page
// calls the hub with fetch too (so streaming works: chat, /v1/native/stream, job progress); operations still go
// through the app's `hub_call` command.
import { reactive } from 'vue'

const KEY = 'mm_token'

export const auth = reactive({ token: sessionStorage.getItem(KEY) || '', error: '' })

export const inTauri = (): boolean => !!(window as any).__TAURI__

/** Call one of the desktop app's commands (Tauri 2: core.invoke; Tauri 1: tauri.invoke). */
export function invoke(cmd: string, args: Record<string, unknown> = {}): Promise<any> {
  const t = (window as any).__TAURI__
  return (t.core?.invoke ?? t.tauri.invoke)(cmd, args)
}

let hubInfoCache: Promise<{ url: string; token: string }> | null = null

/** Where the app's hub is and its token (desktop app only). Waits while the app starts the hub. */
export function hubInfo(): Promise<{ url: string; token: string }> {
  if (!hubInfoCache) {
    hubInfoCache = invoke('hub_info').catch((e) => {
      hubInfoCache = null
      throw typeof e === 'string' ? new Error(e) : e
    })
  }
  return hubInfoCache
}

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
  let root = base()
  if (inTauri()) {
    const info = await hubInfo()
    root = info.url
    headers.set('Authorization', `Bearer ${info.token}`)
  } else {
    headers.set('Authorization', `Bearer ${auth.token}`)
  }
  let r: Response
  try {
    r = await fetch(root + path, { ...init, headers })
  } catch (e) {
    if ((e as any)?.name === 'AbortError') throw e
    if (inTauri()) hubInfoCache = null // the hub may have been restarted on another port
    throw new Error('the hub is not reachable')
  }
  if (inTauri() && (r.status === 401 || r.status === 403)) {
    hubInfoCache = null // a restarted hub has a new token; the next call asks the app again
    return r
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

/** Fetch from the hub with the right base URL and bearer token (browser: same origin and the signed-in token;
 *  desktop app: the app's hub). Returns the Response; check `ok` yourself. */
export function hubFetch(path: string, init: RequestInit = {}): Promise<Response> {
  return http(path, init)
}

/** Call the hub and read a server-sent event stream (for example POST /v1/chat/completions with stream:true, or
 *  GET /v1/native/stream with no body). `body` null = GET, else POST as JSON. Each `data:` payload is parsed as JSON
 *  and passed to `onEvent` (with the event name as the second argument when the stream gives one); `data: [DONE]`
 *  ends the stream. Resolves when the stream ends; rejects when the hub refuses or is unreachable; aborting `signal`
 *  stops it early and resolves quietly. */
export async function sse(
  path: string,
  body: unknown,
  onEvent: (data: any, event?: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  try {
    const r = await http(path, {
      method: body == null ? 'GET' : 'POST',
      signal,
      headers: { Accept: 'text/event-stream', ...(body == null ? {} : { 'Content-Type': 'application/json' }) },
      body: body == null ? undefined : JSON.stringify(body),
    })
    if (!r.ok || !r.body) throw await failure(r)
    await readSse(r.body.getReader(), onEvent)
  } catch (e) {
    if ((e as any)?.name === 'AbortError' || signal?.aborted) return
    throw e
  }
}

async function readSse(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  onEvent: (data: any, event?: string) => void,
): Promise<void> {
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
      const lines = block.split(/\r?\n/)
      const data = lines
        .filter((l) => l.startsWith('data:'))
        .map((l) => l.slice(5).replace(/^ /, ''))
        .join('\n')
      const ev = lines.find((l) => l.startsWith('event:'))?.slice(6).trim()
      if (!data) continue
      if (data.trim() === '[DONE]') return
      try {
        onEvent(JSON.parse(data), ev)
      } catch (_) {
        /* a broken line */
      }
    }
  }
}

/** Run one hub operation (the same names MCP and POST /v1/call/{op} use). */
export async function call(op: string, args: Record<string, unknown> = {}): Promise<any> {
  if (inTauri()) {
    try {
      return await invoke('hub_call', { op, args })
    } catch (e) {
      throw typeof e === 'string' ? new Error(e) : e
    }
  }
  const r = await http(`/v1/call/${op}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(args),
  })
  if (!r.ok) throw await failure(r)
  return (await r.json()).result
}

export type LiveState = 'connecting' | 'live' | 'down'

/** Live trace events: server-sent events (read with fetch so the token stays in a header), in the browser and in the
 *  desktop app. Returns a function that stops it. */
export function subscribe(onEvent: (e: any) => void, onState: (s: LiveState) => void): () => void {
  const stop = new AbortController()
  const run = async () => {
    let wait = 1000
    while (!stop.signal.aborted) {
      if (!inTauri() && !auth.token) {
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
        await readSse(r.body.getReader(), (e) => onEvent(e))
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

/** Send a chat request that is traced; the answer itself is not used here (the Thoughts page shows it live). */
export async function chatTraced(model: string, prompt: string, signal: AbortSignal): Promise<void> {
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

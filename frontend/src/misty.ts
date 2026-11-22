// Typed access to Misty's operations (misty.*, src/misty/ops.rs; docs/misty/DESIGN.md) over the dashboard's transport.
// A run is followed by polling misty.run.get with wait_ms, which works in a browser and in the desktop app alike.
import { call } from '@/transport'

export type Role = 'reflex' | 'fast' | 'main' | 'deep' | 'ear' | 'eye' | 'embed' | 'rerank'

export interface Resolution {
  role: Role; model: string | null; status: 'ready' | 'missing' | 'unfit' | 'unavailable'
  source: string; size_bytes: number; note: string; loaded?: boolean
}

export interface MistyStatus {
  ready: boolean
  roster: Resolution[]
  cactus: { available: boolean; library: string | null; models: string[]; error: string | null; archives: { role: string; file: string; present: boolean; loaded: boolean }[] }
  engines: { stock_q2: boolean; prism: boolean; llama_version: string | null }
  free_ram_mb: number
  rag: { collections: { collection: string; sources: number; chunks: number }[] }
  policy: Record<string, string>
  roots: string[]
  runs_active: number
  approvals_pending: number
  next: string[]
}

export interface Component { id: string; what: string; kind: string; size_bytes: number; installed: boolean; default: boolean; from: string; note: string }
export interface SetupPlan { components: Component[]; default: string[]; default_download_bytes: number; how: string }

export interface Session { id: string; title: string; created: number; updated: number; messages: number }
export interface Approval { approval: string; run: string; tool: string; class: string; args: any }

/** One entry of a run's event log (the `type` says which fields it has; see agent.rs). */
export interface RunEvent { type: string; t_ms?: number; [k: string]: any }

export interface RunPage { run: string; events: RunEvent[]; next: number; done: boolean; running: boolean; pending: Approval[]; text?: string; status?: string }

export interface Ask {
  input: string; session?: string; new_session?: boolean; role?: 'fast' | 'main' | 'deep'; rag?: boolean; collection?: string
  tools?: string[]; no_tools?: boolean; panel?: boolean; approvals?: 'ask' | 'deny'
}

export interface Hit { chunk: number; collection: string; source: string; start_line: number; end_line: number; text: string; score: number; coverage: number; via: string }
export interface Source { collection: string; source: string; bytes: number; chunks: number; vectors: number }
export interface ToolInfo { name: string; class: string; policy: string; description: string }
export interface Pipeline { name: string; description?: string; steps: any[]; [k: string]: any }

export const ROLE_LABEL: Record<string, string> = {
  reflex: 'Reflex (Needle3)', fast: 'Fast', main: 'Main', deep: 'Deep', ear: 'Ear (Whistle)', eye: 'Eye (Mirage)', embed: 'Embeddings', rerank: 'Rerank',
}

export const misty = {
  status: (): Promise<MistyStatus> => call('misty.status', {}),
  doctor: (): Promise<any> => call('misty.doctor', {}),
  setupPlan: (): Promise<SetupPlan> => call('misty.setup.plan', {}),
  setupRun: (install: string[] = ['default']): Promise<any> => call('misty.setup.run', { install }),
  bench: (tokens = 64): Promise<any> => call('misty.bench', { tokens }),
  start: (a: Ask): Promise<{ run: string; session: string | null }> => call('misty.run.start', a as any),
  /** Events after `from`; waits up to waitMs for new ones (the hub caps it at 25 s). */
  get: (run: string, from = 0, waitMs = 0): Promise<RunPage> => call('misty.run.get', { run, from, wait_ms: waitMs }),
  cancel: (run: string) => call('misty.run.cancel', { run }),
  approve: (approval: string, allow: boolean) => call('misty.approve', { approval, allow }),
  approvals: async (): Promise<Approval[]> => (await call('misty.approvals', {})).pending || [],
  sessions: async (limit = 50): Promise<Session[]> => (await call('misty.session.list', { limit })).sessions || [],
  newSession: async (title = 'chat'): Promise<string> => (await call('misty.session.new', { title })).session,
  session: async (session: string): Promise<{ role: string; content: string }[]> => (await call('misty.session.get', { session })).messages || [],
  rename: (session: string, title: string) => call('misty.session.rename', { session, title }),
  remove: (session: string) => call('misty.session.delete', { session }),
  route: (input: string): Promise<any> => call('misty.route', { input }),
  ingest: (path: string, collection?: string): Promise<any> => call('misty.rag.ingest', { path, ...(collection ? { collection } : {}) }),
  search: async (query: string, collection?: string, k = 6): Promise<Hit[]> =>
    (await call('misty.rag.search', { query, k, ...(collection ? { collection } : {}) })).hits || [],
  sources: async (collection?: string): Promise<{ sources: Source[]; collections: { collection: string; sources: number; chunks: number }[] }> =>
    call('misty.rag.sources', collection ? { collection } : {}),
  removeSource: (source: string, collection?: string) => call('misty.rag.remove', { source, ...(collection ? { collection } : {}) }),
  tools: async (): Promise<{ tools: ToolInfo[]; policy: Record<string, string> }> => call('misty.tools.list', {}),
  config: async (): Promise<{ config: Record<string, any>; path: string }> => call('misty.config.get', {}),
  setConfig: (patch: Record<string, unknown>) => call('misty.config.set', { patch }),
  pipelines: async (): Promise<Pipeline[]> => (await call('misty.pipeline.list', {})).pipelines || [],
  runPipeline: (name: string, inputs: Record<string, unknown>, wait = true): Promise<any> => call('misty.pipeline.run', { name, inputs, wait }),
}

/** What a run's events add up to: the answer so far, the reasoning, which role answered, tool calls, citations. */
export interface Citation { n?: number; source: string; start_line?: number; end_line?: number; [k: string]: any }
export interface Folded {
  text: string; reasoning: string; role: string; model: string; notes: string[]; tools: RunEvent[]; error: string
  citations: Citation[]; why: string[]; status: string; ms: number; usage: Record<string, number> | null
}

export function fold(events: RunEvent[]): Folded {
  const f: Folded = { text: '', reasoning: '', role: '', model: '', notes: [], tools: [], error: '', citations: [], why: [], status: '', ms: 0, usage: null }
  for (const e of events) {
    switch (e.type) {
      case 'token': f.text += e.text || ''; break
      case 'reasoning': f.reasoning += e.text || ''; break
      case 'model': f.role = e.role || f.role; f.model = e.model || f.model; break
      case 'tool': f.tools.push(e); break
      case 'escalate': f.notes.push(`moved up from ${e.from} to ${e.to}: ${e.why}`); f.text = ''; f.reasoning = ''; break
      case 'reflex': if (e.accepted) f.notes.push(`Needle chose the tool call (confidence ${Number(e.confidence).toFixed(2)})`); break
      case 'transcript': f.notes.push(`heard: ${e.text}`); break
      case 'vision': f.notes.push(`image ${e.image}: ${e.description}`); break
      case 'error': f.error = e.message || 'the run failed'; break
      case 'route': f.why = Array.isArray(e.reasons) ? e.reasons.map(String) : []; if (e.role) f.role = e.role; break
      case 'retrieve': f.notes.push(`searched the index: ${e.used} of ${e.hits} passages used`); break
      case 'final':
        f.text = e.text ?? f.text
        f.citations = Array.isArray(e.citations) ? e.citations : []
        f.status = e.status || ''
        f.ms = e.ms || 0
        f.usage = e.usage || null
        if (e.role) f.role = e.role
        if (e.error && !f.error) f.error = e.error
        break
    }
  }
  return f
}

/** Follows a run until it ends, calling `update` with every page. Stops quietly when `stop()` says so. */
export async function follow(run: string, update: (p: RunPage, all: RunEvent[]) => void, stop: () => boolean = () => false): Promise<RunEvent[]> {
  const all: RunEvent[] = []
  let from = 0
  for (;;) {
    const p = await misty.get(run, from, 20000)
    from = p.next
    all.push(...p.events)
    update(p, all)
    if (p.done || stop()) return all
  }
}

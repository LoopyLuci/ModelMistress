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

/** What a class of tool does when Misty calls it: runs at once, waits for the owner, or is refused. */
export type Policy = 'auto' | 'ask' | 'deny'
export type ToolClass = 'read' | 'write' | 'exec' | 'net' | 'hub'

export const TOOL_CLASSES: { id: ToolClass; label: string; what: string }[] = [
  { id: 'read', label: 'Read', what: 'list, read and search files and the index; look things up' },
  { id: 'write', label: 'Write', what: 'create and change files, add to the index' },
  { id: 'exec', label: 'Commands', what: 'run commands in the working folder' },
  { id: 'net', label: 'Network', what: 'fetch web pages' },
  { id: 'hub', label: 'Models', what: 'load and unload models' },
]

/** A named working setup: a folder, more folders, variables for commands, and a policy per class of tool. */
export interface Environment {
  id: string
  name: string
  description: string
  cwd: string | null
  roots: string[]
  vars: Record<string, string>
  /** A class left out keeps Misty's own policy. */
  policy: Partial<Record<ToolClass, Policy>>
  created: number
  updated: number
}
/** What `misty.env.save` takes: no id makes a new one. */
export type EnvironmentInput = Partial<Omit<Environment, 'created' | 'updated'>> & { name: string }

export interface Session {
  id: string; title: string; created: number; updated: number; messages: number
  /** The session's environment (id and name) and its own working folder, when it has them. */
  env?: string | null; env_name?: string | null; cwd?: string | null
}
/** What `misty.session.set` answers: the session's choices and what they add up to. */
export interface SessionScope {
  session: string; env: string | null; env_name: string | null; cwd: string | null
  scope: { environment: string | null; cwd: string | null; roots: string[]; policy: Record<ToolClass, Policy> }
}
export interface Browse {
  path: string | null; parent: string | null; truncated: boolean
  dirs: { name: string; path: string }[]
  places: { label: string; path: string }[]
}
export interface Approval { approval: string; run: string; tool: string; class: string; args: any; warning?: string | null }

/** One entry of a run's event log (the `type` says which fields it has; see agent.rs). */
export interface RunEvent { type: string; t_ms?: number; [k: string]: any }

export interface RunPage { run: string; events: RunEvent[]; next: number; done: boolean; running: boolean; pending: Approval[]; text?: string; status?: string }

export interface Ask {
  input: string; session?: string; new_session?: boolean; role?: 'fast' | 'main' | 'deep'; rag?: boolean; collection?: string
  tools?: string[]; no_tools?: boolean; panel?: boolean; approvals?: 'ask' | 'deny'
  /** Work in this environment (id or name) and/or this folder: remembered by the session, which they change. */
  env?: string; cwd?: string
}

export interface Hit { chunk: number; collection: string; source: string; start_line: number; end_line: number; text: string; score: number; coverage: number; via: string }
export interface Source { collection: string; source: string; bytes: number; chunks: number; vectors: number }
export interface ToolInfo { name: string; class: string; policy: string; description: string }
export interface Pipeline { name: string; description?: string; steps: any[]; [k: string]: any }

/** The owner's verdict on a reply (src/misty/feedback.rs). The wire names match the Android app's export. */
export type Label = 'good' | 'bad' | 'no_tools' | 'needs_tools' | 'corrected'
/** The verdicts a reply can get from the buttons (corrected is given with the right answer). */
export const LABELS: { id: Label; text: string }[] = [
  { id: 'good', text: 'Good' },
  { id: 'bad', text: 'Bad' },
  { id: 'no_tools', text: 'No tool needed' },
  { id: 'needs_tools', text: 'Should have used a tool' },
]
export const LABEL_TEXT: Record<Label, string> = {
  good: 'good', bad: 'bad', no_tools: 'no tool needed', needs_tools: 'should have used a tool', corrected: 'corrected',
}
export interface FeedbackCounts { total: number; good: number; bad: number; no_tools: number; needs_tools: number; corrected: number; training: number }
export interface Verdict { id: number; question: string; answer: string; label: Label; tools: string[]; correction: string | null; at: number }
export interface FeedbackList { counts: FeedbackCounts; verdicts: Verdict[] }
export interface RateResult { id: number; label: Label; counts: FeedbackCounts }
/** A row of the hub's run log: its session (if any) and the start of its question (200 characters). */
export interface RunRow { id: string; session: string | null; input: string; role?: string | null; status: string; started: number; ms: number }
/** One line of a chat. For Misty's replies, `run` is the run that made it (what a rating is about) and `rated` the verdict given. */
export interface Turn { role: string; content: string; run?: string; rated?: Label }

/**
 * Gives each of a session's replies the run that made it. Each question is matched, in order, with the session's finished
 * runs, oldest first; a run that failed or was cancelled gives no reply, so it is not matched. The run log keeps only the
 * first 200 characters of a question, which is how a question is matched.
 */
export function pairRuns(turns: Turn[], runs: RunRow[], session: string): Turn[] {
  const oldest = runs.filter((r) => r.session === session && r.status === 'done').reverse()
  const out = turns.map((t) => ({ ...t }))
  let cursor = 0
  for (let k = 0; k < out.length; k++) {
    if (out[k].role !== 'user') continue
    const want = out[k].content.slice(0, 200)
    const j = oldest.findIndex((r, i) => i >= cursor && r.input === want)
    if (j < 0) continue
    cursor = j + 1
    if (out[k + 1]?.role === 'assistant') out[k + 1].run = oldest[j].id
  }
  return out
}

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
  /** Choose a session's environment and folder: `null` clears one, a key left out stays. */
  setScope: (session: string, p: { env?: string | null; cwd?: string | null }): Promise<SessionScope> => call('misty.session.set', { session, ...p }),
  envs: async (): Promise<Environment[]> => (await call('misty.env.list', {})).environments || [],
  saveEnv: async (e: EnvironmentInput): Promise<Environment> => (await call('misty.env.save', e as any)).environment,
  deleteEnv: (id: string) => call('misty.env.delete', { id }),
  /** The folders inside `path` (hidden ones too when asked); with no path, the usual starting places. */
  browse: (path?: string, hidden = false): Promise<Browse> => call('misty.browse', { ...(path ? { path } : {}), ...(hidden ? { hidden } : {}) }),
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
  /** The owner's verdict on a finished run's reply. `correction` is the right answer, given with `corrected`. */
  rate: (run: string, label: Label, correction?: string): Promise<RateResult> =>
    call('misty.rate', { run, label, ...(correction ? { correction } : {}) }),
  feedback: (limit = 100): Promise<FeedbackList> => call('misty.feedback.list', { limit }),
  forget: (id: number): Promise<{ removed: number }> => call('misty.feedback.forget', { id }),
  forgetAll: (): Promise<{ removed: number }> => call('misty.feedback.forget', { all: true }),
  /** The good replies and the corrections, one JSON object per line. */
  training: (): Promise<{ count: number; jsonl: string }> => call('misty.training.export', {}),
  /** Recent runs, newest first. */
  runs: async (limit = 300): Promise<RunRow[]> => (await call('misty.run.list', { limit })).recent || [],
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

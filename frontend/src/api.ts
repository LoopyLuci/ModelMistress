// Typed access to the Knowledge Module (km.*) and factory (factory.*) operations, over the dashboard's transport.
// Shapes follow docs/native/km-runtime.md and src/km/ops.rs; the factory's follow docs/native/km-factory.md §2.
import { call } from '@/transport'

export interface Tag { tag: string; origin: string; confidence: number; approved: boolean; problem?: string | null }

export interface KmItem {
  key: string; id: string; version: string; kind: string; name: string; description: string; domain: string
  tags: string[]; bytes: number; content_id: string; signature: string; source: string; misdeclared: string | null
  base?: string | null; base_version?: string | null; arch?: string | null
  has_centroid: boolean; channels: string[]; resident: boolean; pinned: boolean; banned: boolean
  consolidation: Record<string, { state?: string; uplift?: number; last_step?: number; steps?: number }> | null
}

export interface ScoreTerms {
  vec: { raw: number; weight: number; value: number; used: boolean }
  lex: { raw: number; weight: number; value: number }
  route: { raw: number; weight: number; value: number }
  prior: { value: number }
  pin: { pinned: boolean; value: number }
  tags: { matched: number; confidence_sum: number; weight: number; value: number }
}

export interface SearchHit {
  rank: number; key: string; id: string; version: string; kind: string; name: string; domain: string; score: number
  terms: ScoreTerms; notes: string[]; version_chosen_by: string
}

export interface Search {
  weights: Record<string, number>; embedder: any; notes: string[]; results: SearchHit[]
  excluded: { key: string; reasons: string[] }[]
}

export interface Resident {
  key: string; id: string; kind: string; tier: 'ram' | 'vram'; core: string | null; bytes: number; ram: number; vram: number
  heat: number; idle_s: number; in_flight: number; pinned: boolean; attach: string; loaded_at: number
}

export interface Residency {
  tiers: Record<'disk' | 'ram' | 'vram', { budget: number; used: number; count?: number; free?: number | null }>
  cores: Record<string, { ram: { budget: number; used: number }; vram: { budget: number; used: number } }>
  half_life_s: number; resident: Resident[]; pinned: string[]; note?: string
}

export interface KmEvent {
  seq: number; at: number; kind: string; key: string; tier: string | null; bytes: number; reason: string; detail: any
}

export interface DataRecord {
  id: string; rev: number; status: 'pending' | 'approved' | 'rejected'; task?: string
  input?: { role: string; content: string }[]; output?: string
  media?: { kind: string; sha256?: string; path?: string; url?: string }[]
  labels?: Record<string, any>; tags?: string[]
  source?: { kind?: string; model?: string; trace?: string; [k: string]: any }
  split?: string; edited_by?: string; [k: string]: any
}

export interface Revision { rev: number; at: number; action: string; by: string | null; note: string | null; record: DataRecord }

export interface Vocabulary { path: string; custom: boolean; namespaces: Record<string, any> }

// ---- Knowledge modules -------------------------------------------------------------------------------------------
export const km = {
  list: (a: Record<string, unknown> = {}): Promise<KmItem[]> => call('km.list', a),
  show: (id: string, version?: string): Promise<any> => call('km.show', { id, ...(version ? { version } : {}), history: 30 }),
  search: (a: Record<string, unknown>): Promise<Search> => call('km.search', a),
  load: (id: string, version?: string, extra: Record<string, unknown> = {}) => call('km.load', { id, version, ...extra }),
  unload: (id: string, version?: string) => call('km.unload', { id, version }),
  unloadAll: () => call('km.unload', { all: true }),
  pin: (id: string, pinned: boolean) => call('km.pin', { id, pinned }),
  ban: (id: string, banned: boolean, reason?: string) => call('km.ban', { id, banned, reason }),
  channelSet: (id: string, channel: string, version: string, expect?: string) =>
    call('km.channel.set', { id, channel, version, ...(expect != null ? { expect } : {}) }),
  channelGet: (id: string) => call('km.channel.get', { id }),
  export: (id: string, version?: string) => call('km.export', { id, version }),
  remove: (id: string, version: string) => call('km.remove', { id, version }),
  residency: (): Promise<Residency> => call('km.residency'),
  events: (a: { since?: number; limit?: number; kind?: string } = {}): Promise<{ last_seq: number | null; events: KmEvent[] }> =>
    call('km.events', a),
  /** A text file of an installed KM (path as in km.show payload), at most max_bytes (ceiling 1 MiB). */
  fileGet: (id: string, path: string, version?: string, max_bytes = 20000): Promise<{ key: string; path: string; bytes: number; truncated: boolean; text: string }> =>
    call('km.file.get', { id, path, ...(version ? { version } : {}), max_bytes }),
  vocabularyGet: (): Promise<Vocabulary> => call('km.vocabulary.get'),
  vocabularyEdit: (a: Record<string, unknown>) => call('km.vocabulary.edit', a),
  data: {
    /** Filters (all server-side): status, split, tag, task, text, min_quality, source_kind, limit, offset. */
    list: (id: string, a: Record<string, unknown> = {}): Promise<any> => call('km.data.list', { id, ...a }),
    get: (id: string, record: string, history = true): Promise<DataRecord & { history?: Revision[] }> =>
      call('km.data.get', { id, record, history }),
    edit: (id: string, record: string, changes: Record<string, unknown>, a: Record<string, unknown> = {}) =>
      call('km.data.edit', { id, record, changes, by: 'owner', ...a }),
    approve: (id: string, records: string[], note?: string) => call('km.data.approve', { id, records, by: 'owner', note }),
    approveFrom: (id: string, from: string) => call('km.data.approve', { id, from_status: from, by: 'owner' }),
    reject: (id: string, records: string[], note?: string) => call('km.data.reject', { id, records, by: 'owner', note }),
    commit: (id: string, version?: string) => call('km.data.commit', { id, ...(version ? { version } : {}) }),
  },
}

// ---- Factory (src/factory/ops.rs). The backend may not be there: pages show "not available" on a 404 -----------
export interface FactoryUsage { tokens?: number; prompt_tokens?: number; completion_tokens?: number; calls?: number; retries?: number; wall_ms?: number }

export interface FactoryTask {
  id: string; name?: string; goal: string; state: string; status: string; step?: string | null; created_ms?: number; updated_ms?: number
  error?: string | null; records?: number; usage?: FactoryUsage; [k: string]: any
}

/** The create-task JSON (factory.task.create). */
export interface FactorySpec {
  goal: string; kinds: string[]; domain: string; sources: Record<string, string>[]; author: string; reviewer?: string
  base?: string; size: { records?: number; max_tokens?: number }; tagging: { vocabulary: string; review: string }
  approval: string; budget: { wall_s?: number; max_records?: number; gpu_hours?: number }
  device?: Record<string, string>; plan_approval?: boolean; eval_sets?: Record<string, unknown>[]; think?: boolean
  per_call?: number; name?: string; start?: boolean; license?: string
}

export interface ReviewItem {
  id: number; type: 'record' | 'tag' | 'vocab' | 'promotion'; kind: string; task: string; ref: string; record_id: any; tag: any
  confidence: number | null; origin: any; record: any; reason: string; data: any; state: string; decision: string | null
  decided_by: string | null; note: string | null; created: number; decided: number | null
}

export interface FactorySettings {
  embedder: string; embed_model: string; promotion: string; min_uplift: number; auto_add_vocabulary: boolean
  review_threshold: number; tag_threshold: number; tag_floor: number; dedupe_threshold: number; leak_threshold: number
  chunk_chars: number; max_file_bytes: number; max_files: number; max_total_bytes: number; max_url_bytes: number
  max_retries: number; gen_max_tokens: number; eval_notes: number
}

export const FACTORY_STEPS = ['plan', 'gather', 'generate', 'verify', 'tag', 'build', 'evaluate', 'publish']
/** Task states: created, running, awaiting_approval, budget, failed, cancelled, interrupted, done. */
export const FACTORY_STATES = ['created', 'running', 'awaiting_approval', 'budget', 'failed', 'cancelled', 'interrupted', 'done']
export const stateLabel = (st: string) => (st === 'awaiting_approval' ? 'awaiting approval' : st === 'budget' ? 'budget reached' : st)
/** Whether a task in this state can be resumed (the hub refuses a running or a done one). */
export const resumable = (st: string) => ['budget', 'interrupted', 'failed', 'cancelled', 'created', 'awaiting_approval'].includes(st)

/** Step-based progress, 0..100: finished steps of the eight, plus a half for the one running. */
export function stepProgress(state: string, step?: string | null, steps?: { name: string; status: string }[]): number {
  if (state === 'done') return 100
  let done: number
  if (steps && steps.length) {
    done = steps.filter((x) => x.status === 'done' || x.status === 'skipped').length
    if (steps.some((x) => x.status === 'running')) done += 0.5
  } else {
    const i = FACTORY_STEPS.indexOf(String(step || ''))
    done = i < 0 ? 0 : i + (state === 'running' ? 0.5 : state === 'awaiting_approval' ? 1 : 0)
  }
  return Math.round(Math.max(0, Math.min(100, (done / FACTORY_STEPS.length) * 100)))
}

export const fmtDuration = (ms?: number | null) => {
  const s = Math.round((ms || 0) / 1000)
  return s < 60 ? `${s} s` : s < 3600 ? `${Math.floor(s / 60)} min ${s % 60} s` : `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`
}

export const factory = {
  create: (spec: FactorySpec): Promise<any> => call('factory.task.create', spec as any),
  /** factory.task.list answers a plain array. */
  list: async (a: Record<string, unknown> = {}): Promise<FactoryTask[]> => {
    const r = await call('factory.task.list', a)
    return Array.isArray(r) ? r : r?.items || r?.tasks || []
  },
  /** records: how many records to include as record_items; review: include the open review items. */
  get: (id: string, a: { records?: number; logs?: number; review?: boolean; status?: string } = {}): Promise<any> =>
    call('factory.task.get', { id, ...a }),
  /** plan replaces the plan; changes is a merge patch (null removes a key). */
  planEdit: (id: string, a: { plan?: unknown; changes?: Record<string, unknown>; approve?: boolean }) => call('factory.task.plan.edit', { id, ...a }),
  /** from re-opens that step and the ones after it; changes may raise budgets or change models first. */
  resume: (id: string, a: { from?: string; changes?: Record<string, unknown> } = {}) => call('factory.task.resume', { id, ...a }),
  cancel: (id: string) => call('factory.task.cancel', { id }),
  /** {total, items}; state defaults to open (approved, rejected, edited or all show decided ones). */
  reviewList: async (a: Record<string, unknown> = {}): Promise<{ total: number; items: ReviewItem[] }> => {
    const r = await call('factory.review.list', a)
    return Array.isArray(r) ? { total: r.length, items: r } : { total: r?.total ?? (r?.items || []).length, items: r?.items || [] }
  },
  /** item is the review item's number; edit takes changes ({input, output, tags} of a record). */
  decide: (item: number | string, decision: 'approve' | 'edit' | 'reject', extra: Record<string, unknown> = {}) =>
    call('factory.review.decide', { item, decision, by: 'owner', ...extra }),
  settings: (): Promise<FactorySettings> => call('factory.settings', {}),
  setSettings: (settings: Record<string, unknown>): Promise<FactorySettings> => call('factory.settings', { settings }),
  embedderSetup: (model: string, a: Record<string, unknown> = {}): Promise<any> => call('factory.embedder.setup', { model, ...a }),
}

/** True when the hub answered that the operation does not exist (the factory backend is not built in). */
export function unavailable(e: any): boolean {
  return e?.status === 404 || /no operation|unknown operation|not found/i.test(String(e?.message || e))
}

/** The hub's model ids, for the model pickers. */
export async function modelIds(): Promise<string[]> {
  const ms = await call('model.list', {})
  return (ms as any[]).filter((m) => m.source !== 'whisper').map((m) => m.id)
}

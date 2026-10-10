// Typed access to the Knowledge Module (km.*) and factory (factory.*) operations, over the dashboard's transport.
// Shapes follow docs/native/km-runtime.md and src/km/ops.rs; the factory's follow docs/native/km-factory.md §2.
import { call } from '@/transport'

export interface Tag { tag: string; origin: string; confidence: number; approved: boolean; problem?: string | null }

export interface KmItem {
  key: string; id: string; version: string; kind: string; name: string; description: string; domain: string
  tags: string[]; bytes: number; content_id: string; signature: string; source: string; misdeclared: string | null
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
  vocabularyGet: (): Promise<Vocabulary> => call('km.vocabulary.get'),
  vocabularyEdit: (a: Record<string, unknown>) => call('km.vocabulary.edit', a),
  data: {
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

// ---- Factory (km-factory.md §2). The backend may not be there: pages show "not available" on a 404 -------------
export interface FactoryTask {
  id: string; goal: string; status: string; step?: string; progress?: number; created_ms?: number; updated_ms?: number
  error?: string | null; [k: string]: any
}

/** The create-task JSON of km-factory.md §2. */
export interface FactorySpec {
  goal: string; kinds: string[]; domain: string; sources: Record<string, string>[]; author: string; reviewer?: string
  base: string; size: { records?: number; max_tokens?: number }; tagging: { vocabulary: string; review: string }
  approval: string; budget: { gpu_hours?: number }
}

export const FACTORY_STEPS = ['plan', 'gather', 'generate', 'verify', 'tag', 'build', 'evaluate', 'publish']

export const factory = {
  create: (spec: FactorySpec): Promise<any> => call('factory.task.create', spec as any),
  list: async (a: Record<string, unknown> = {}): Promise<FactoryTask[]> => {
    const r = await call('factory.task.list', a)
    return Array.isArray(r) ? r : r?.items || r?.tasks || []
  },
  get: (id: string): Promise<any> => call('factory.task.get', { id }),
  planEdit: (id: string, a: { plan?: unknown; approve?: boolean }) => call('factory.task.plan.edit', { id, ...a }),
  resume: (id: string) => call('factory.task.resume', { id }),
  cancel: (id: string) => call('factory.task.cancel', { id }),
  reviewList: async (a: Record<string, unknown> = {}): Promise<any[]> => {
    const r = await call('factory.review.list', a)
    return Array.isArray(r) ? r : r?.items || []
  },
  decide: (item: string, decision: 'approve' | 'edit' | 'reject', extra: Record<string, unknown> = {}) =>
    call('factory.review.decide', { item, decision, ...extra }),
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

// A model's serving profile (`[profiles."<model id>"]` in the hub's config.toml): how that one model is started.
// The Models page edits it through config.get and config.set; the hub checks the values (config.set answers with the
// reason when one is refused) and a profile wins over the global `[serving]` table and the Settings page.
import { call } from '@/transport'

export type Profile = Record<string, unknown>

export interface Field {
  key: string
  label: string
  kind: 'int' | 'select' | 'tri'
  /** For `select`: the values (the first, blank, means "not set"). */
  options?: string[]
  help: string
  min?: number
}

export const KV_TYPES = ['f32', 'f16', 'bf16', 'q8_0', 'q4_0', 'q4_1', 'iq4_nl', 'q5_0', 'q5_1']

export const FIELDS: Field[] = [
  {
    key: 'engine',
    label: 'Engine',
    kind: 'select',
    options: ['', 'llama.cpp', 'prism', 'rigel'],
    help: 'What runs the model. Blank is the built-in llama.cpp. Ternary Bonsai files and some embedding models need "prism".',
  },
  { key: 'gpu_layers', label: 'GPU layers', kind: 'int', help: '999 puts every layer that fits on the GPU, 0 runs on the CPU. Blank uses the global setting.' },
  { key: 'ctx_size', label: 'Context size (tokens, all slots together)', kind: 'int', min: 0, help: '0 is the model\'s own. Blank uses the global setting.' },
  { key: 'parallel', label: 'Parallel requests', kind: 'int', min: 1, help: 'Slots that answer at the same time; each gets its share of the context.' },
  { key: 'batch_size', label: 'Batch size', kind: 'int', min: 1, help: 'Tokens read per step. An embedding model needs it at least as large as its longest passage.' },
  { key: 'ubatch_size', label: 'Micro-batch size', kind: 'int', min: 1, help: 'The physical batch. Keep it equal to the batch size for an embedding model.' },
  { key: 'flash_attn', label: 'Flash attention', kind: 'select', options: ['', 'auto', 'on', 'off'], help: 'Faster and lighter on memory where the engine has it.' },
  { key: 'cache_type_k', label: 'KV cache type (keys)', kind: 'select', options: ['', ...KV_TYPES], help: 'A smaller type saves memory for a little quality.' },
  { key: 'cache_type_v', label: 'KV cache type (values)', kind: 'select', options: ['', ...KV_TYPES], help: 'A quantised value cache needs flash attention.' },
  { key: 'cache_prompt', label: 'Keep each slot\'s prompt', kind: 'tri', help: 'The next turn only reads what is new.' },
  { key: 'jinja', label: 'Jinja chat template', kind: 'tri', help: 'Needed for tool calls with some models.' },
  {
    key: 'paged',
    label: 'Load from disk if it does not fit',
    kind: 'tri',
    help: 'Slow: a model bigger than the memory is read from disk as it runs, seconds per token. It also needs "Allow a model bigger than the memory to load from disk" in Settings, and you load it yourself; a request never does.',
  },
]

/** The form's values: every field a string, blank when the profile does not set it. */
export type Form = Record<string, string>

export function toForm(p: Profile | undefined): Form {
  const f: Form = {}
  for (const { key, kind } of FIELDS) {
    const v = p?.[key]
    f[key] = v === undefined || v === null ? '' : kind === 'tri' ? (v ? 'true' : 'false') : String(v)
  }
  return f
}

/** The profile a form means: blank fields are left out, so they fall back to the global settings. */
export function toProfile(f: Form, keep: Profile = {}): Profile {
  // fields the form does not show (a profile edited by hand in config.toml) stay as they were
  const shown = new Set(FIELDS.map((x) => x.key))
  const out: Profile = {}
  for (const [k, v] of Object.entries(keep)) if (!shown.has(k)) out[k] = v
  for (const { key, kind } of FIELDS) {
    const raw = String(f[key] ?? '').trim() // a number input can hand back a number
    if (raw === '') continue
    if (kind === 'int') {
      const n = Number(raw)
      if (Number.isFinite(n)) out[key] = Math.trunc(n)
    } else if (kind === 'tri') out[key] = raw === 'true'
    else out[key] = raw
  }
  return out
}

/** Does the profile set anything? */
export const isCustom = (p: Profile | undefined): boolean => !!p && Object.keys(p).length > 0

/** A profile in words: "engine prism, 0 GPU layers, batch 2048". */
export function describe(p: Profile | undefined): string {
  if (!isCustom(p)) return ''
  const bits: string[] = []
  const v = p as Profile
  if (v.engine) bits.push(`engine ${v.engine}`)
  if (v.gpu_layers !== undefined) bits.push(v.gpu_layers === 0 ? 'CPU only' : `${v.gpu_layers} GPU layers`)
  if (v.ctx_size !== undefined) bits.push(`context ${v.ctx_size}`)
  if (v.parallel !== undefined) bits.push(`${v.parallel} slots`)
  if (v.batch_size !== undefined) bits.push(`batch ${v.batch_size}`)
  if (bits.length === 0) bits.push(`${Object.keys(v).length} setting${Object.keys(v).length === 1 ? '' : 's'}`)
  return bits.join(', ')
}

export interface Saved {
  profiles: Record<string, Profile>
  warnings: string[]
}

/** The profiles in config.toml now. */
export async function readProfiles(): Promise<Saved> {
  const c = await call('config.get', {})
  return { profiles: (c.profiles || {}) as Record<string, Profile>, warnings: (c.config_warnings || []) as string[] }
}

/**
 * Save one model's profile (an empty one removes it). The whole table is read first and written back with the one
 * change, because config.set replaces a field as a whole. Rejects with the hub's reason when a value is refused.
 */
export async function saveProfile(id: string, profile: Profile): Promise<Saved> {
  const { profiles } = await readProfiles()
  const next = { ...profiles }
  if (isCustom(profile)) next[id] = profile
  else delete next[id]
  const c = await call('config.set', { settings: { profiles: next } })
  return { profiles: (c.profiles || {}) as Record<string, Profile>, warnings: (c.config_warnings || []) as string[] }
}

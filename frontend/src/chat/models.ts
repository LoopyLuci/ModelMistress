// The hub's models as the Chat and Models pages see them (model.list, model.loaded).
import { call } from '@/transport'

export interface HubModel {
  id: string
  source: string
  size_gb?: number | null
  architecture?: string | null
  name?: string | null
  size_label?: string | null
  file_type?: string | null
  context_length?: number | null
  vision?: boolean
  native?: boolean
  family?: string | null
  experts?: number | null
  experts_used?: number | null
  path?: string
  loaded?: boolean | null
  kind?: string
  task?: string | null
  /** ModelMistress downloaded it, so the hub may delete it (a model another program put on the disk is not). */
  deletable?: boolean
  /** The quantisation the file names (`Q5_K_XL`, `PQ2_0`), else the header's; files of one name differ in it. */
  quant?: string | null
  /** A name that tells models apart: the name in the file and the quantisation. */
  label?: string
  /** What it can run on, where it runs now (loaded) or would go, and the memory that takes. */
  hardware?: {
    engine: string
    can_run_on: ('gpu' | 'cpu')[]
    runs_on: 'gpu' | 'split' | 'cpu' | 'none' | 'paged' | 'unknown'
    /** true: loaded, `runs_on` is measured; false: where a load would go. */
    now: boolean
    gpu_share?: number | null
    vram_mib?: number | null
    ram_mib?: number | null
    device?: string | null
    /** "GPU", "CPU", "GPU 80% + CPU" or "does not fit". */
    chip: string
    label: string
  }
  /** Not a file: the model router, which sends each message to the model that suits it. */
  router?: boolean
  /** For the router: at least one role has a model that can answer now (a new chat starts on it only then). */
  ready?: boolean
}

export interface LoadedModel {
  model: string
  pid: number
  port: number
  uptime_s: number
  idle_s: number
  embeddings: boolean
  reranking: boolean
  args: string[]
  log?: string
  /** How it is served; `placement` is where the weights went, `placement_note` why when that is not what was asked. */
  serving?: { placement?: 'gpu' | 'partial' | 'cpu' | 'unknown'; placement_note?: string | null; vram_mib?: number | null }
}

const notChat = ['clip', 'sd', 'sd3', 'flux', 'whisper', 'wavtokenizer-dec']

/** Models a chat can talk to: not whisper, vision nets, image or embedding-only models. */
export const isChatModel = (m: HubModel): boolean =>
  m.source !== 'whisper' &&
  m.source !== 'vision' &&
  !m.kind &&
  !(m.architecture && notChat.includes(m.architecture)) &&
  !/embed|rerank/i.test(m.id)

export async function listModels(refresh = false): Promise<HubModel[]> {
  const ms = (await call('model.list', { include_ollama: true, ...(refresh ? { refresh: true } : {}) })) as HubModel[]
  return ms
}

export async function loadedModels(): Promise<LoadedModel[]> {
  return (await call('model.loaded', {})) as LoadedModel[]
}

export interface Group { label: string; models: HubModel[] }

/** The router as a choice in a chat's model list: the model name `auto`. */
export const routerChoice = (ready = true): HubModel => ({ id: 'auto', source: 'router', router: true, ready })

/**
 * The models a chat can choose from: the router first when the hub has one, then the chat models. The Models page
 * shows files only, so the router is added here and not in `listModels`.
 */
export async function chatChoices(refresh = false): Promise<HubModel[]> {
  const files = await listModels(refresh)
  let router: HubModel[] = []
  try {
    // router.status answers only on a hub that has the router (it comes with Misty)
    const st = (await call('router.status', {})) as { enabled?: boolean; roles?: { status: string }[] }
    if (st.enabled !== false) router = [routerChoice((st.roles || []).some((r) => r.status === 'ready'))]
  } catch (_) {
    /* an older hub, or one without Misty: no router in the list */
  }
  return [...router, ...files]
}

/** The picker's groups: the router first, then MM's own models, then the rest by where they are from. */
export function groupModels(ms: HubModel[]): Group[] {
  const by = (f: (m: HubModel) => boolean) => ms.filter(f).sort((a, b) => a.id.localeCompare(b.id))
  const groups: Group[] = [
    { label: 'Router', models: by((m) => !!m.router) },
    { label: 'MM models', models: by((m) => !m.router && !!m.native) },
    { label: 'Local models', models: by((m) => !m.router && !m.native && !m.source.startsWith('ollama')) },
    { label: 'Ollama', models: by((m) => !m.router && !m.native && m.source.startsWith('ollama')) },
  ]
  return groups.filter((g) => g.models.length)
}

export const modelLabel = (m: HubModel): string =>
  m.router ? 'Auto: the router picks the model for each message' : `${m.id}${m.size_gb ? ` (${m.size_gb} GB)` : ''}${m.loaded ? ' - loaded' : ''}`

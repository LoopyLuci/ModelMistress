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

/** The picker's groups: MM's own models first, then the rest by where they are from. */
export function groupModels(ms: HubModel[]): Group[] {
  const by = (f: (m: HubModel) => boolean) => ms.filter(f).sort((a, b) => a.id.localeCompare(b.id))
  const groups: Group[] = [
    { label: 'MM models', models: by((m) => !!m.native) },
    { label: 'Local models', models: by((m) => !m.native && !m.source.startsWith('ollama')) },
    { label: 'Ollama', models: by((m) => !m.native && m.source.startsWith('ollama')) },
  ]
  return groups.filter((g) => g.models.length)
}

export const modelLabel = (m: HubModel): string =>
  `${m.id}${m.size_gb ? ` (${m.size_gb} GB)` : ''}${m.loaded ? ' - loaded' : ''}`

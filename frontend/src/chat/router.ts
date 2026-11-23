// The model router as the Chat and Models pages use it: `auto` (any chat), router.status, router.pick and the role
// choices (the `roles` of Misty's config). The decision itself is made by the hub (src/misty/auto.rs).
import { call } from '@/transport'
import type { ChatMessage } from './hub'
import { toWire } from './hub'

/** The model name that asks the hub's router. */
export const AUTO = 'auto'

export type BrainRole = 'fast' | 'main' | 'deep'
export type RoleStatus = 'ready' | 'missing' | 'unfit' | 'unavailable' | 'not_allowed'

/** One role as the hub sees it: which model fills it and whether it can answer now. */
export interface RoleView {
  role: string
  model: string | null
  status: RoleStatus
  /** `config` (your choice), `preferred` (a model the router prefers by name) or `fallback` (an installed model of the right size). */
  source: 'config' | 'preferred' | 'fallback' | string
  loaded: boolean
  vision?: boolean
  usable?: boolean
  note: string
  score?: number | null
}

export interface RouterStatus {
  model: string
  enabled: boolean
  roles: RoleView[]
  /** The models retrieval uses (embeddings, reranking), chosen in the same place. */
  support: RoleView[]
  overrides: Record<string, string>
  free_ram_mb: number
}

export interface Pick {
  role: BrainRole | null
  model: string | null
  forced: BrainRole | null
  reasons: string[]
  candidates: RoleView[]
}

/** What a reply keeps of the router's decision, so the chat can say who answered and why. */
export interface RouteNote {
  role: string
  model: string
  why: string[]
}

export const BRAIN_ROLES: BrainRole[] = ['fast', 'main', 'deep']

export const ROLE_HELP: Record<string, string> = {
  fast: 'short, simple turns: greetings, lookups, small questions',
  main: 'most requests: writing, code, documents',
  deep: 'hard reasoning and long inputs',
  embed: 'turns text into vectors for searching your documents',
  rerank: 'orders search results (optional)',
}

export const SOURCE_LABEL: Record<string, string> = {
  config: 'your choice',
  preferred: 'a model the router prefers',
  fallback: 'picked by size',
}

export const STATUS_TEXT: Record<string, string> = {
  ready: 'ready',
  missing: 'no model',
  unfit: 'does not fit the free memory',
  unavailable: 'needs an engine that is not installed',
  not_allowed: 'not allowed',
}

export const routerStatus = (): Promise<RouterStatus> => call('router.status', {})

/** Which model the router would send a conversation to. Nothing is sent to it. */
export const pickFor = (messages: ChatMessage[]): Promise<Pick> => call('router.pick', { messages: toWire(messages) })

/** The same for one message typed into the tester on the Models page. */
export const pickText = (input: string, hasImage = false): Promise<Pick> =>
  call('router.pick', { input, ...(hasImage ? { has_image: true } : {}) })

/** Choose the model for a role, or `null` to go back to the router's own choice (a JSON merge patch). */
export async function setRole(role: string, model: string | null): Promise<void> {
  await call('misty.config.set', { patch: { roles: { [role]: model } } })
}

const TAG = /^@(fast|main|deep)$/i

/**
 * The message without `@fast`, `@main` and `@deep` (the words that ask the router for a role); every other
 * character stays as it was. The hub does the same for `auto`; this is for a message sent straight to the picked model.
 */
export function withoutRoleTags(text: string): string {
  let out = ''
  let skip = false
  let removed = false
  for (const part of text.split(/(\s)/)) {
    if (skip && /^\s$/.test(part)) {
      skip = false
      continue
    }
    skip = false
    if (TAG.test(part)) {
      removed = true
      skip = true // the space that followed the tag goes with it
      continue
    }
    out += part
  }
  return removed ? out.trim() : text
}

/** The conversation as it goes to the picked model: the role tags of the last user message are not sent on. */
export function withoutTagsInLastUserMessage(messages: ChatMessage[]): ChatMessage[] {
  const i = messages.map((m) => m.role).lastIndexOf('user')
  if (i < 0) return messages
  const out = messages.slice()
  out[i] = { ...out[i], content: withoutRoleTags(out[i].content) }
  return out
}

/** One line for a reply: "main, qwen3-8b". */
export const routeLine = (r: RouteNote): string => `${r.role}: ${r.model}`

/** The models a role may be given in the picker: language models for the three brains, embedders and rerankers by name. */
export function suitsRole(role: string, id: string, isChat: boolean): boolean {
  const embedLike = /embed|bge|e5-|gte-|nomic/i.test(id)
  if (role === 'embed') return embedLike && !/rerank/i.test(id)
  if (role === 'rerank') return /rerank/i.test(id)
  return isChat && !embedLike
}

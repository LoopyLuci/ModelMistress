// Conversations are kept in this browser (or the app's webview) only, in localStorage.
import { reactive, watch } from 'vue'
import type { ChatMessage, ChatParams, ReplyStats } from './hub'

export interface Msg extends ChatMessage {
  stats?: ReplyStats
  traceId?: string
  model?: string
  error?: string
  stopped?: boolean
}

export interface Conversation {
  id: string
  title: string
  renamed?: boolean
  model: string
  system: string
  params: ChatParams
  trace: boolean
  messages: Msg[]
  updated: number
}

const KEY = 'mm_chats'
const LAST = 'mm_chat_last'

export const defaults = (): ChatParams => ({ temperature: 0.7, top_p: 0.95, max_tokens: 0 })

function load(): Conversation[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || '[]')
    return Array.isArray(v) ? v : []
  } catch (_) {
    return []
  }
}

export const chats = reactive({ list: load() as Conversation[], error: '' })

let timer: number | undefined
function save() {
  // Several saves while a reply streams collapse into one.
  window.clearTimeout(timer)
  timer = window.setTimeout(flush, 400)
}

export function flush() {
  window.clearTimeout(timer)
  let list = chats.list
  for (;;) {
    try {
      localStorage.setItem(KEY, JSON.stringify(list))
      chats.error = list.length < chats.list.length ? 'The browser storage is full: the oldest conversations were not kept.' : ''
      return
    } catch (_) {
      if (list.length <= 1) {
        chats.error = 'The browser storage is full: this conversation is not kept.'
        return
      }
      list = [...list].sort((a, b) => b.updated - a.updated).slice(0, list.length - 1)
    }
  }
}

watch(() => chats.list, save, { deep: true })
window.addEventListener('beforeunload', flush)

export function newConversation(model = '', from?: Conversation): Conversation {
  const c: Conversation = {
    id: `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
    title: 'New chat',
    model: model || from?.model || '',
    system: from?.system || '',
    params: from ? { ...from.params } : defaults(),
    trace: from?.trace || false,
    messages: [],
    updated: Date.now(),
  }
  chats.list.unshift(c)
  return chats.list[0]
}

export function removeConversation(id: string) {
  const i = chats.list.findIndex((c) => c.id === id)
  if (i >= 0) chats.list.splice(i, 1)
}

export const sorted = (): Conversation[] => [...chats.list].sort((a, b) => b.updated - a.updated)
export const lastId = {
  get: () => localStorage.getItem(LAST) || '',
  set: (id: string) => localStorage.setItem(LAST, id),
}

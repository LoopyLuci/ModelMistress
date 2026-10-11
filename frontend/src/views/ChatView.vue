<template>
  <div class="chat" :class="{ side: sideOpen }">
    <aside class="list" aria-label="Conversations">
      <div class="row lh">
        <button class="btn primary grow" type="button" @click="startNew">New chat</button>
        <button class="btn closer" type="button" aria-label="Close the list" @click="sideOpen = false">Close</button>
      </div>
      <p v-if="chats.error" class="error-box small">{{ chats.error }}</p>
      <ul>
        <li v-for="c in list" :key="c.id" :class="{ on: conv && c.id === conv.id }">
          <template v-if="renaming === c.id">
            <input ref="renameBox" v-model="renameText" type="text" class="grow" aria-label="Chat name" @keydown.enter.prevent="saveRename(c)" @keydown.esc.prevent="renaming = ''" @blur="saveRename(c)" />
          </template>
          <template v-else>
            <button class="pick" type="button" :title="c.title" @click="open(c.id)">
              <span class="t">{{ c.title }}</span>
              <span class="muted small">{{ c.model || 'no model' }}</span>
            </button>
            <button class="ico" type="button" :aria-label="`Rename ${c.title}`" title="Rename" @click="startRename(c)">Rename</button>
            <button class="ico" type="button" :aria-label="`Delete ${c.title}`" title="Delete" @click="askDelete = c.id">Delete</button>
          </template>
        </li>
      </ul>
      <p v-if="!list.length" class="muted small pad">No conversations yet.</p>
    </aside>

    <section v-if="conv" class="main">
      <header class="bar">
        <button class="btn menu" type="button" @click="sideOpen = true">Chats</button>
        <label class="sr-only" for="modelpick">Model</label>
        <select id="modelpick" v-model="conv.model" class="modelpick" :disabled="busy">
          <option v-if="conv.model && !known(conv.model)" :value="conv.model">{{ conv.model }} (not found)</option>
          <option v-if="!conv.model" value="" disabled>Choose a model</option>
          <optgroup v-for="g in groups" :key="g.label" :label="g.label">
            <option v-for="m in g.models" :key="m.id" :value="m.id">{{ modelLabel(m) }}</option>
          </optgroup>
        </select>
        <span v-if="cur?.native" class="pill good">MM model</span>
        <span v-if="cur?.vision" class="pill">vision</span>
        <span class="grow"></span>
        <label class="row small" title="Record every token and its alternatives for this chat's replies"><input v-model="conv.trace" type="checkbox" /> trace replies</label>
        <button class="btn" type="button" :aria-expanded="showSettings" @click="showSettings = !showSettings">Settings</button>
      </header>

      <div v-if="showSettings" class="settings card">
        <label class="field">System prompt
          <textarea v-model="conv.system" rows="3" placeholder="Optional. Tells the model how to behave."></textarea>
        </label>
        <div class="formgrid">
          <label class="field">Temperature: {{ conv.params.temperature }}
            <input v-model.number="conv.params.temperature" type="range" min="0" max="2" step="0.05" />
          </label>
          <label class="field">Top-p: {{ conv.params.top_p }}
            <input v-model.number="conv.params.top_p" type="range" min="0.05" max="1" step="0.05" />
          </label>
          <label class="field">Max tokens (0 = the model's own limit)
            <input v-model.number="conv.params.max_tokens" type="number" min="0" step="64" />
          </label>
        </div>
      </div>

      <div ref="scroller" class="msgs" role="log" aria-live="polite" @scroll="onScroll">
        <div v-if="!conv.messages.length" class="empty">
          <h2>Chat with your models</h2>
          <p class="muted">
            Pick a model above and write below. A model that is not loaded yet loads on the first message;
            large ones take a minute. <span v-if="!allModels.length && loaded">No models were found: add a folder in Settings.</span>
          </p>
        </div>
        <MessageView
          v-for="(m, i) in conv.messages"
          :key="i"
          :msg="m"
          :streaming="busy && i === conv.messages.length - 1 && m.role === 'assistant'"
          :waiting="waiting"
          :can-retry="!busy && i === conv.messages.length - 1 && m.role === 'assistant'"
          @retry="retry"
        />
      </div>

      <form class="composer" @submit.prevent="send">
        <p v-if="error" class="error-box small" role="alert">{{ error }}</p>
        <div v-if="pending.length" class="thumbs">
          <span v-for="(u, i) in pending" :key="i" class="thumb"><img :src="u" alt="attached picture" /><button type="button" aria-label="Remove picture" @click="pending.splice(i, 1)">x</button></span>
        </div>
        <div class="row inrow">
          <textarea ref="box" v-model="input" rows="2" class="grow" placeholder="Message (Enter sends, Shift+Enter for a new line)" aria-label="Message" @keydown="onKey" @paste="onPaste"></textarea>
          <input v-if="cur?.vision" ref="file" type="file" accept="image/*" multiple hidden @change="onFiles" />
          <button v-if="cur?.vision" class="btn" type="button" title="Attach a picture" @click="file?.click()">Picture</button>
          <button v-if="busy" class="btn danger" type="button" @click="stop">Stop</button>
          <button v-else class="btn primary" type="submit" :disabled="!canSend">Send</button>
        </div>
      </form>
    </section>

    <ConfirmDialog :open="!!askDelete" title="Delete this conversation?" action="Delete" @confirm="doDelete" @cancel="askDelete = ''">
      It is removed from this browser; nothing else is affected.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import MessageView from '@/components/MessageView.vue'
import { useRoute, useRouter } from 'vue-router'
import { call } from '@/transport'
import { splitThink } from '@/chat/markdown'
import { streamChat, type ChatMessage } from '@/chat/hub'
import { groupModels, isChatModel, listModels, modelLabel, type HubModel } from '@/chat/models'
import { chats, flush, lastId, newConversation, removeConversation, sorted, type Conversation, type Msg } from '@/chat/store'

const route = useRoute()
const router = useRouter()
const MODEL_KEY = 'mm_chat_model'
const conv = ref<Conversation | null>(null)
const allModels = ref<HubModel[]>([])
const loaded = ref(false)
const input = ref('')
const pending = ref<string[]>([])
const busy = ref(false)
const error = ref('')
const sideOpen = ref(false)
const showSettings = ref(false)
const askDelete = ref('')
const renaming = ref('')
const renameText = ref('')
const renameBox = ref<HTMLInputElement[] | HTMLInputElement | null>(null)
const scroller = ref<HTMLElement | null>(null)
const box = ref<HTMLTextAreaElement | null>(null)
const file = ref<HTMLInputElement | null>(null)
const waitSecs = ref(0)
let abort: AbortController | null = null
let waitTimer: number | undefined
let stick = true

const list = computed(() => sorted())
const chatModels = computed(() => allModels.value.filter(isChatModel))
const groups = computed(() => groupModels(chatModels.value))
const cur = computed(() => allModels.value.find((m) => m.id === conv.value?.model))
const known = (id: string) => allModels.value.some((m) => m.id === id)
const canSend = computed(() => !busy.value && !!conv.value?.model && (!!input.value.trim() || pending.value.length > 0))
const waiting = computed(() =>
  waitSecs.value < 3 ? 'Waiting for the model...' : `Waiting for the model... ${waitSecs.value} s (a model that is not loaded yet is loading)`,
)

async function refreshModels(refresh = false) {
  try {
    allModels.value = await listModels(refresh)
  } catch (e: any) {
    error.value = `Could not list models: ${e?.message || e}`
  }
  loaded.value = true
  pickDefault()
}

function pickDefault() {
  const c = conv.value
  if (!c || (c.model && known(c.model)) || !chatModels.value.length) return
  const want = localStorage.getItem(MODEL_KEY)
  const m =
    chatModels.value.find((x) => x.id === want) ||
    chatModels.value.find((x) => x.native && x.loaded) ||
    chatModels.value.find((x) => x.loaded) ||
    chatModels.value.find((x) => x.native) ||
    chatModels.value[0]
  if (!c.model) c.model = m.id
}

function open(id: string) {
  const c = chats.list.find((x) => x.id === id)
  if (!c) return
  conv.value = c
  lastId.set(id)
  sideOpen.value = false
  error.value = ''
  pickDefault()
  stick = true
  nextTick(() => scrollDown(true))
}

function startNew() {
  const from = conv.value || undefined
  // An empty chat is reused rather than piling up.
  const empty = chats.list.find((c) => !c.messages.length)
  if (empty) return open(empty.id)
  const c = newConversation(from?.model, from)
  open(c.id)
  nextTick(() => box.value?.focus())
}

function startRename(c: Conversation) {
  renaming.value = c.id
  renameText.value = c.title
  nextTick(() => {
    const el = Array.isArray(renameBox.value) ? renameBox.value[0] : renameBox.value
    el?.focus()
    el?.select()
  })
}
function saveRename(c: Conversation) {
  if (renaming.value !== c.id) return
  const t = renameText.value.trim()
  if (t) {
    c.title = t
    c.renamed = true
  }
  renaming.value = ''
}

function doDelete() {
  const id = askDelete.value
  askDelete.value = ''
  removeConversation(id)
  flush()
  if (conv.value?.id === id) {
    const next = sorted()[0]
    if (next) open(next.id)
    else {
      conv.value = null
      startNew()
    }
  }
}

function scrollDown(force = false) {
  const el = scroller.value
  if (el && (force || stick)) el.scrollTop = el.scrollHeight
}
function onScroll() {
  const el = scroller.value
  if (el) stick = el.scrollHeight - el.scrollTop - el.clientHeight < 60
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    if (canSend.value) send()
  }
}
function onWindowKey(e: KeyboardEvent) {
  if (e.key === 'Escape' && busy.value) {
    e.preventDefault()
    stop()
  }
}

// ---- pictures (vision models) ---------------------------------------------------------------------------------
async function shrink(f: File): Promise<string> {
  const url = URL.createObjectURL(f)
  try {
    const img = await new Promise<HTMLImageElement>((ok, no) => {
      const i = new Image()
      i.onload = () => ok(i)
      i.onerror = () => no(new Error('not a picture'))
      i.src = url
    })
    const s = Math.min(1, 1024 / Math.max(img.width, img.height))
    const c = document.createElement('canvas')
    c.width = Math.max(1, Math.round(img.width * s))
    c.height = Math.max(1, Math.round(img.height * s))
    c.getContext('2d')!.drawImage(img, 0, 0, c.width, c.height)
    return c.toDataURL('image/jpeg', 0.85)
  } finally {
    URL.revokeObjectURL(url)
  }
}
async function addFiles(fs: File[]) {
  for (const f of fs) {
    if (!f.type.startsWith('image/')) continue
    try {
      pending.value.push(await shrink(f))
    } catch (e: any) {
      error.value = `${f.name}: ${e?.message || e}`
    }
  }
}
async function onFiles(e: Event) {
  const el = e.target as HTMLInputElement
  await addFiles([...(el.files || [])])
  el.value = ''
}
function onPaste(e: ClipboardEvent) {
  if (!cur.value?.vision) return
  const fs = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'))
  if (fs.length) {
    e.preventDefault()
    addFiles(fs)
  }
}

// ---- sending ----------------------------------------------------------------------------------------------------
function historyFor(c: Conversation, upto: number): ChatMessage[] {
  const out: ChatMessage[] = []
  if (c.system.trim()) out.push({ role: 'system', content: c.system.trim() })
  for (const m of c.messages.slice(0, upto)) {
    if (m.error && !m.content) continue
    out.push({
      role: m.role,
      content: m.role === 'assistant' ? splitThink(m.content).answer : m.content,
      ...(m.images?.length ? { images: m.images } : {}),
    })
  }
  return out
}

async function send() {
  const c = conv.value
  if (!c || !canSend.value) return
  error.value = ''
  const text = input.value.trim()
  c.messages.push({ role: 'user', content: text, ...(pending.value.length ? { images: [...pending.value] } : {}) })
  if (!c.renamed && c.messages.length === 1) c.title = (text || 'Picture').replace(/\s+/g, ' ').slice(0, 48)
  input.value = ''
  pending.value = []
  await reply(c)
}

async function retry() {
  const c = conv.value
  if (!c || busy.value) return
  while (c.messages.length && c.messages[c.messages.length - 1].role === 'assistant') c.messages.pop()
  if (c.messages.length) await reply(c)
}

async function reply(c: Conversation) {
  localStorage.setItem(MODEL_KEY, c.model)
  c.messages.push({ role: 'assistant', content: '', reasoning: '', model: c.model })
  const idx = c.messages.length - 1
  const a = (): Msg => c.messages[idx]
  c.updated = Date.now()
  busy.value = true
  waitSecs.value = 0
  const t0 = Date.now()
  waitTimer = window.setInterval(() => (waitSecs.value = Math.round((Date.now() - t0) / 1000)), 1000)
  abort = new AbortController()
  stick = true
  nextTick(() => scrollDown(true))
  try {
    const stats = await streamChat(c.model, historyFor(c, idx), c.params, c.trace, abort.signal, {
      onContent: (d) => {
        a().content += d
        nextTick(() => scrollDown())
      },
      onReasoning: (d) => {
        a().reasoning = (a().reasoning || '') + d
        nextTick(() => scrollDown())
      },
    })
    a().stats = stats
  } catch (e: any) {
    if (e?.name === 'AbortError') a().stopped = true
    else a().error = String(e?.message || e)
  } finally {
    busy.value = false
    window.clearInterval(waitTimer)
    abort = null
    c.updated = Date.now()
    if (!a().reasoning) delete a().reasoning
    if (c.trace && (a().content || a().reasoning)) findTrace(c, idx, t0)
    flush()
    refreshModels()
    nextTick(() => box.value?.focus())
  }
}

function stop() {
  abort?.abort()
}

/** The hub does not name the trace in the reply, so the newest trace of this model since the request started is it. */
async function findTrace(c: Conversation, idx: number, since: number) {
  for (let n = 0; n < 4; n++) {
    try {
      const l = await call('trace.list', { model: c.model, limit: 5 })
      const t = (l.items as any[]).find((x) => x.started_ms >= since - 3000)
      if (t) {
        c.messages[idx].traceId = t.id
        return
      }
    } catch (_) {
      return
    }
    await new Promise((ok) => setTimeout(ok, 600))
  }
}

onMounted(async () => {
  window.addEventListener('keydown', onWindowKey)
  const last = lastId.get()
  const c = chats.list.find((x) => x.id === last) || sorted()[0]
  if (c) open(c.id)
  else startNew()
  await refreshModels()
  const want = route.query.model
  if (typeof want === 'string' && want) {
    // from the Models page: a fresh chat with that model
    if (conv.value?.messages.length) startNew()
    if (conv.value) conv.value.model = want
    router.replace({ path: '/' })
  }
  nextTick(() => box.value?.focus())
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onWindowKey)
  window.clearInterval(waitTimer)
  // A reply in progress goes on only while this page is open.
  abort?.abort()
})
watch(
  () => conv.value?.messages.length,
  () => nextTick(() => scrollDown()),
)
</script>

<style scoped>
.chat { display: grid; grid-template-columns: 260px minmax(0, 1fr); height: 100%; min-height: 0; }
.list { border-right: 1px solid var(--border); background: var(--panel); display: flex; flex-direction: column; min-height: 0; overflow-y: auto; }
.lh { padding: 10px; flex: none; }
.closer, .menu { display: none; }
.list ul { list-style: none; padding: 0 6px 10px; }
.list li { display: flex; align-items: center; gap: 2px; border-radius: 8px; padding: 2px; }
.list li.on { background: var(--panel-2); }
.list li:hover { background: var(--panel-2); }
.pick { flex: 1 1 auto; min-width: 0; text-align: left; background: none; border: 0; padding: 5px 8px; display: flex; flex-direction: column; }
.pick .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pick .small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ico { display: none; background: none; border: 0; font-size: 0.72rem; color: var(--muted); padding: 2px 4px; }
.list li:hover .ico, .list li:focus-within .ico { display: inline; }
.ico:hover { color: var(--text); text-decoration: underline; }
.list input[type='text'] { width: 100%; }
.pad { padding: 0 12px; }
.main { display: flex; flex-direction: column; min-width: 0; min-height: 0; height: 100%; }
.bar { display: flex; align-items: center; gap: 8px; padding: 8px 14px; border-bottom: 1px solid var(--border); flex: none; flex-wrap: wrap; }
.modelpick { min-width: 0; max-width: 100%; width: 340px; }
.settings { margin: 8px 14px 0; flex: none; }
.msgs { flex: 1 1 auto; min-height: 0; overflow-y: auto; padding: 14px; display: flex; flex-direction: column; gap: 10px; }
.msgs > * { max-width: 900px; width: 100%; margin: 0 auto; }
.empty { margin-top: 12vh; text-align: center; }
.empty h2 { margin-bottom: 8px; }
.composer { flex: none; border-top: 1px solid var(--border); padding: 10px 14px; background: var(--panel); }
.composer > * { max-width: 900px; margin-left: auto; margin-right: auto; }
.inrow { flex-wrap: nowrap; align-items: stretch; }
.inrow textarea { resize: none; }
.thumbs { display: flex; gap: 6px; flex-wrap: wrap; margin-bottom: 6px; }
.thumb { position: relative; }
.thumb img { height: 56px; border-radius: 6px; border: 1px solid var(--border); display: block; }
.thumb button { position: absolute; top: -6px; right: -6px; width: 18px; height: 18px; border-radius: 50%; border: 1px solid var(--border); background: var(--panel); line-height: 1; font-size: 0.7rem; }
@media (max-width: 800px) {
  .chat { grid-template-columns: minmax(0, 1fr); }
  .list { position: fixed; inset: 0 25% 0 0; z-index: 20; transform: translateX(-102%); transition: transform 0.15s; box-shadow: 2px 0 14px rgba(0, 0, 0, 0.3); }
  .chat.side .list { transform: none; }
  .closer, .menu { display: inline-block; }
  .ico { display: inline; }
  .modelpick { width: 100%; order: 3; flex: 1 1 100%; }
  .bar { padding: 6px 10px; }
  .msgs, .composer { padding-left: 8px; padding-right: 8px; }
}
</style>

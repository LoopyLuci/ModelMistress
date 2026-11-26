<template>
  <div class="chat" :class="{ side: sideOpen, withcanvas: canvasOpen }">
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
        <span v-if="cur?.router" class="pill good" title="Each message goes to the fast, main or deep model that suits it">router</span>
        <span v-if="cur?.native" class="pill good">MM model</span>
        <span v-if="cur?.vision" class="pill">vision</span>
        <span class="grow"></span>
        <label class="row small" title="Record every token and its alternatives for this chat's replies"><input v-model="conv.trace" type="checkbox" /> trace replies</label>
        <button class="btn" type="button" :aria-pressed="canvasOpen" :title="assets.length ? 'Pictures, video, audio, 3D and files from this chat' : 'Nothing to show yet'" @click="canvasOpen = !canvasOpen">Canvas<span v-if="assets.length" class="count"> {{ assets.length }}</span></button>
        <button class="btn" type="button" :aria-expanded="showSettings" @click="showSettings = !showSettings">Settings</button>
      </header>

      <RouterStrip v-if="cur?.router" :refresh="replies" />

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
          <p v-if="cur?.router" class="muted">
            Auto sends each message to the fast, main or deep model that suits it, and each reply says which answered and why.
            Put <span class="mono">@fast</span>, <span class="mono">@main</span> or <span class="mono">@deep</span> in a message to choose.
          </p>
        </div>
        <MessageView
          v-for="(m, i) in conv.messages"
          :key="i"
          :msg="m"
          :streaming="busy && i === conv.messages.length - 1 && m.role === 'assistant'"
          :waiting="waiting"
          :can-retry="!busy && i === conv.messages.length - 1 && m.role === 'assistant'"
          :index="i"
          @retry="retry"
          @open-asset="openAsset"
        />
      </div>

      <div v-if="behind" class="jumprow">
        <button class="btn" type="button" @click="jumpToLatest">Jump to latest</button>
      </div>

      <form class="composer" @submit.prevent="send">
        <p v-if="error" class="error-box small" role="alert">{{ error }}</p>
        <div v-if="pending.length" class="thumbs">
          <span v-for="(u, i) in pending" :key="i" class="thumb"><img :src="u" alt="attached picture" /><button type="button" aria-label="Remove picture" @click="pending.splice(i, 1)">x</button></span>
        </div>
        <ul v-if="queuedHere.length" class="queued" aria-label="Queued messages">
          <li v-for="(q, i) in queuedHere" :key="i">
            <span class="muted small">Next:</span>
            <span class="qtext">{{ q.text || 'Picture' }}</span>
            <button class="mini" type="button" :aria-label="`Remove queued message ${i + 1}`" @click="unqueue(q)">Remove</button>
          </li>
        </ul>
        <div class="row inrow">
          <textarea ref="box" v-model="input" rows="2" class="grow" placeholder="Message (Enter sends, Shift+Enter for a new line, Up brings back an earlier message)" aria-label="Message" @keydown="onKey" @paste="onPaste"></textarea>
          <input v-if="canPicture" ref="file" type="file" accept="image/*" multiple hidden @change="onFiles" />
          <button v-if="canPicture" class="btn" type="button" :title="cur?.router ? 'Attach a picture (the router then needs a model that can see pictures)' : 'Attach a picture'" @click="file?.click()">Picture</button>
          <button class="btn" type="button" :aria-pressed="listening" :disabled="transcribing" :title="listening ? 'Stop, and write down what was said' : 'Say the message instead of typing it'" @click="toggleDictation">{{ listening ? 'Stop listening' : transcribing ? 'Transcribing...' : 'Dictate' }}</button>
          <button v-if="busy" class="btn danger" type="button" @click="stop">Stop</button>
          <button v-if="busy" class="btn primary" type="submit" :disabled="!canQueue" title="Sent after this reply">Queue</button>
          <button v-else class="btn primary" type="submit" :disabled="!canSend">Send</button>
        </div>
      </form>
    </section>

    <MediaCanvas v-if="conv && canvasOpen" class="canvaspane" :assets="assets" :active-id="activeAsset" @close="canvasOpen = false" @select="activeAsset = $event" />

    <ConfirmDialog :open="!!askDelete" title="Delete this conversation?" action="Delete" @confirm="doDelete" @cancel="askDelete = ''">
      It is removed from this browser; nothing else is affected.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import MessageView from '@/components/MessageView.vue'
import MediaCanvas from '@/components/MediaCanvas.vue'
import RouterStrip from '@/components/RouterStrip.vue'
import { conversationAssets } from '@/chat/media'
import { useRoute, useRouter } from 'vue-router'
import { call } from '@/transport'
import { splitThink } from '@/chat/markdown'
import { streamChat, type ChatMessage } from '@/chat/hub'
import { chatChoices, groupModels, isChatModel, modelLabel, type HubModel } from '@/chat/models'
import { AUTO, pickFor, withoutTagsInLastUserMessage, type RouteNote } from '@/chat/router'
import { chats, flush, lastId, newConversation, removeConversation, sorted, type Conversation, type Msg } from '@/chat/store'
import { createRecall, entriesFor, loadSent, recallKey, remember } from '@/chat/recall'
import { notInBuild, startRecording, transcribeClip } from '@/speech'

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
const canvasOpen = ref(false)
const activeAsset = ref('')
/** Everything in the conversation that is not text; the canvas shows it, whichever model made it. */
const assets = computed(() => conversationAssets(conv.value?.messages || []))
function openAsset(id: string) {
  activeAsset.value = id
  canvasOpen.value = true
}
// a new picture, video, sound or model that a reply produces opens in the canvas by itself, once
let seenAssets = 0
watch(
  () => assets.value.length,
  (n) => {
    if (n > seenAssets && !busy.value) {
      activeAsset.value = assets.value[n - 1].id
      canvasOpen.value = true
    }
    seenAssets = n
  },
)
watch(() => conv.value?.id, () => { seenAssets = assets.value.length; activeAsset.value = '' })
const askDelete = ref('')
const renaming = ref('')
const renameText = ref('')
const renameBox = ref<HTMLInputElement[] | HTMLInputElement | null>(null)
const scroller = ref<HTMLElement | null>(null)
const box = ref<HTMLTextAreaElement | null>(null)
const file = ref<HTMLInputElement | null>(null)
const waitSecs = ref(0)
/** What the current reply waits for: the router's choice, then the model it chose (or the one picked by hand). */
const routing = ref(false)
const target = ref('')
/** Counts finished replies: the router strip refreshes to show which models the router has loaded. */
const replies = ref(0)
let abort: AbortController | null = null
let waitTimer: number | undefined
let stick = true
/** Messages written during a reply: each waits for the reply and goes next, in the chat it was written in. */
const queue = ref<{ conv: string; text: string; images: string[] }[]>([])
const queuedHere = computed(() => queue.value.filter((q) => q.conv === conv.value?.id))
/** The reader has scrolled up from the newest message: a button brings them back. */
const behind = ref(false)
const listening = ref(false)
const transcribing = ref(false)
let recording: { stop: () => Promise<Blob> } | null = null

/** Up and Down in the message box: what was sent in other chats, then in this one (see chat/recall.ts). */
const recall = createRecall(() =>
  entriesFor(
    (conv.value?.messages || []).filter((m) => m.role === 'user').map((m) => m.content),
    loadSent(),
  ),
)
watch(input, (t) => recall.edited(t))

const list = computed(() => sorted())
const chatModels = computed(() => allModels.value.filter(isChatModel))
const groups = computed(() => groupModels(chatModels.value))
const cur = computed(() => allModels.value.find((m) => m.id === conv.value?.model))
const known = (id: string) => allModels.value.some((m) => m.id === id)
/** A picture can be attached to a model that sees pictures, and to the router (it picks one that does, or says it has none). */
const canPicture = computed(() => !!cur.value?.vision || !!cur.value?.router)
const canSend = computed(() => !busy.value && !!conv.value?.model && (!!input.value.trim() || pending.value.length > 0))
const canQueue = computed(() => busy.value && !!conv.value?.model && (!!input.value.trim() || pending.value.length > 0))
const waiting = computed(() => {
  if (routing.value) return 'The router is choosing a model...'
  const who = target.value ? `Waiting for ${target.value}...` : 'Waiting for the model...'
  return waitSecs.value < 3 ? who : `${who} ${waitSecs.value} s (a model that is not loaded yet is loading)`
})

async function refreshModels(refresh = false) {
  try {
    allModels.value = await chatChoices(refresh)
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
    chatModels.value.find((x) => x.router && x.ready) || // a new chat starts on the router, which picks a model for each message
    chatModels.value.find((x) => x.native && x.loaded) ||
    chatModels.value.find((x) => x.loaded) ||
    chatModels.value.find((x) => x.native) ||
    chatModels.value.find((x) => !x.router) || // the router is first in the list but is no default while it has nothing to send to
    chatModels.value[0]
  if (!c.model) c.model = m.id
}

function open(id: string) {
  const c = chats.list.find((x) => x.id === id)
  if (!c) return
  conv.value = c
  lastId.set(id)
  recall.reset()
  sideOpen.value = false
  error.value = ''
  pickDefault()
  stick = true
  nextTick(() => scrollDown(true))
  nextTick(drainQueue) // a message queued here while a reply ran in another chat goes now
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
  behind.value = !stick
}
function jumpToLatest() {
  stick = true
  behind.value = false
  scrollDown(true)
}

function onKey(e: KeyboardEvent) {
  if (box.value && recallKey(e, box.value, recall, (t) => (input.value = t))) return
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault()
    if (canSend.value || canQueue.value) send()
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
  if (!canPicture.value) return
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
  if (!c) return
  const text = input.value.trim()
  if (busy.value) {
    // a message written during a reply waits for it, and is sent next in this chat
    if (!canQueue.value) return
    queue.value.push({ conv: c.id, text, images: [...pending.value] })
    remember(text)
    recall.reset()
    input.value = ''
    pending.value = []
    return
  }
  if (!canSend.value) return
  error.value = ''
  const images = [...pending.value]
  remember(text)
  recall.reset()
  input.value = ''
  pending.value = []
  await sendNow(c, text, images)
}

async function sendNow(c: Conversation, text: string, images: string[]) {
  c.messages.push({ role: 'user', content: text, ...(images.length ? { images } : {}) })
  if (!c.renamed && c.messages.length === 1) c.title = (text || 'Picture').replace(/\s+/g, ' ').slice(0, 48)
  await reply(c)
}

/** Sends the next message queued in the chat on screen, once no reply is running. */
function drainQueue() {
  const c = conv.value
  if (!c || busy.value) return
  const i = queue.value.findIndex((q) => q.conv === c.id)
  if (i < 0) return
  const [next] = queue.value.splice(i, 1)
  sendNow(c, next.text, next.images)
}

function unqueue(item: { conv: string; text: string; images: string[] }) {
  queue.value = queue.value.filter((q) => q !== item)
}

/** Dictation: the first press starts the microphone; the second writes what was said into the box. */
async function toggleDictation() {
  error.value = ''
  if (listening.value) {
    listening.value = false
    transcribing.value = true
    try {
      const clip = await recording?.stop()
      recording = null
      const said = clip ? await transcribeClip(clip) : ''
      if (said) input.value = [input.value.trimEnd(), said].filter(Boolean).join(' ')
      else error.value = 'No speech was recognised.'
    } catch (e: any) {
      error.value = notInBuild(e) ? 'Dictation is not available in this build.' : `Dictation: ${e?.message || e}`
    } finally {
      transcribing.value = false
    }
    nextTick(() => box.value?.focus())
    return
  }
  try {
    recording = await startRecording()
    listening.value = true
  } catch (e: any) {
    error.value = `The microphone is not available: ${e?.message || e}`
  }
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
  let model = c.model
  let history = historyFor(c, idx)
  target.value = model
  try {
    if (model === AUTO) {
      // The router decides before anything is sent, so the reply can say which model is answering (and which one
      // is loading, when it was not loaded yet). The request then goes to that model.
      routing.value = true
      target.value = ''
      const d = await pickFor(history)
      routing.value = false
      if (!d.model) throw new Error(`The router has no model to send this to: ${d.reasons.join('; ') || 'no model is ready'}. Choose models on the Models page, under Router.`)
      model = d.model
      target.value = model
      history = withoutTagsInLastUserMessage(history)
      const note: RouteNote = { role: d.role || '', model, why: d.reasons }
      a().model = model
      a().route = note
    }
    const stats = await streamChat(model, history, c.params, c.trace, abort.signal, {
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
    routing.value = false
    target.value = ''
    window.clearInterval(waitTimer)
    abort = null
    c.updated = Date.now()
    if (!a().reasoning) delete a().reasoning
    if (c.trace && (a().content || a().reasoning)) findTrace(c, idx, t0, a().model || model)
    flush()
    replies.value++
    refreshModels()
    nextTick(() => box.value?.focus())
    nextTick(drainQueue)
  }
}

function stop() {
  abort?.abort()
}

/** The hub does not name the trace in the reply, so the newest trace of this model since the request started is it. */
async function findTrace(c: Conversation, idx: number, since: number, model: string) {
  for (let n = 0; n < 4; n++) {
    try {
      const l = await call('trace.list', { model, limit: 5 })
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
  if (recording) {
    void recording.stop() // the microphone is let go
    recording = null
  }
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
.chat.withcanvas { grid-template-columns: 260px minmax(0, 1fr) minmax(320px, 42%); }
.canvaspane { min-height: 0; }
.count { color: var(--accent); font-weight: 600; }
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
.queued { list-style: none; margin: 0 auto 6px; padding: 0; max-width: 900px; }
.queued li { display: flex; gap: 8px; align-items: center; }
.qtext { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.jumprow { flex: none; text-align: center; padding: 4px 0; }
.thumb button { position: absolute; top: -6px; right: -6px; width: 18px; height: 18px; border-radius: 50%; border: 1px solid var(--border); background: var(--panel); line-height: 1; font-size: 0.7rem; }
@media (max-width: 800px) {
  .chat, .chat.withcanvas { grid-template-columns: minmax(0, 1fr); }
  .canvaspane { position: fixed; inset: 8% 0 0 0; z-index: 30; border-top: 1px solid var(--border); box-shadow: 0 -2px 14px rgba(0, 0, 0, 0.3); }
  .list { position: fixed; inset: 0 25% 0 0; z-index: 20; transform: translateX(-102%); transition: transform 0.15s; box-shadow: 2px 0 14px rgba(0, 0, 0, 0.3); }
  .chat.side .list { transform: none; }
  .closer, .menu { display: inline-block; }
  .ico { display: inline; }
  .modelpick { width: 100%; order: 3; flex: 1 1 100%; }
  .bar { padding: 6px 10px; }
  .msgs, .composer { padding-left: 8px; padding-right: 8px; }
}
</style>

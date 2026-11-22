<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Misty</h2>
      <span v-if="status" class="pill" :class="status.ready ? 'ok' : 'warn'" role="status">{{ status.ready ? 'ready' : 'needs setup' }}</span>
      <router-link class="btn" to="/misty/system">Setup and status</router-link>
    </div>
    <p v-if="status && !status.ready" class="error-box" role="alert">
      No language model is ready for Misty yet. Open <router-link to="/misty/system">Setup and status</router-link> to install one
      (<span class="mono">model-mistress misty setup</span> does the same).
    </p>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>

    <div class="layout">
      <aside class="card sessions" aria-label="sessions">
        <div class="row">
          <h3 class="grow">Sessions</h3>
          <button class="btn" type="button" :disabled="busy" @click="fresh">New</button>
        </div>
        <ul>
          <li v-for="s in sessions" :key="s.id" :class="{ on: s.id === session }">
            <button type="button" class="link" :disabled="busy" @click="open(s.id)">
              {{ s.title || 'chat' }}
              <span class="muted small">{{ s.messages }} messages</span>
            </button>
            <button type="button" class="mini" :disabled="busy" :aria-label="`Delete ${s.title}`" title="Delete" @click="drop(s.id)">x</button>
          </li>
        </ul>
        <p v-if="!sessions.length" class="muted small">No sessions yet. Ask something.</p>
      </aside>

      <section class="chat">
        <div ref="scroller" class="thread card" aria-live="polite" aria-label="conversation">
          <article v-for="(m, i) in history" :key="i" class="msg" :class="m.role">
            <strong>{{ m.role === 'user' ? 'You' : 'Misty' }}</strong>
            <div class="text">{{ m.content }}</div>
          </article>

          <article v-if="live" class="msg assistant">
            <strong v-if="busy || live.text">Misty</strong>
            <span v-if="live.role" class="pill">{{ live.role }}<template v-if="live.model"> &middot; {{ live.model }}</template></span>
            <details v-if="live.reasoning" class="think">
              <summary>Reasoning <span class="muted small">({{ live.reasoning.length }} characters)</span></summary>
              <div class="text muted">{{ live.reasoning }}</div>
            </details>
            <ul v-if="live.notes.length" class="notes">
              <li v-for="(n, i) in live.notes" :key="i" class="muted small">{{ n }}</li>
            </ul>
            <ul v-if="live.tools.length" class="notes">
              <li v-for="(t, i) in live.tools" :key="i" class="small">
                <span class="mono">{{ t.name }}</span>
                <span class="pill" :class="t.status === 'ok' ? 'ok' : t.status === 'error' || t.status === 'denied' ? 'bad' : ''">{{ t.status }}</span>
                <span v-if="t.note" class="muted"> {{ t.note }}</span>
              </li>
            </ul>
            <div v-if="live.text" class="text">{{ live.text }}</div>
            <p v-else-if="busy && !pending.length" class="muted">Working...</p>
            <p v-if="live.error" class="error-box" role="alert">{{ live.error }}</p>
            <ol v-if="live.citations.length" class="cites small">
              <li v-for="c in live.citations" :key="c.n"><span class="mono">{{ c.source }}<template v-if="c.start_line">:{{ c.start_line }}</template></span></li>
            </ol>
            <footer v-if="!busy && live.ms" class="muted small">
              {{ (live.ms / 1000).toFixed(1) }} s<template v-if="live.usage && live.usage.completion_tokens"> &middot; {{ live.usage.completion_tokens }} tokens</template>
              <template v-if="live.why.length"> &middot; routed: {{ live.why.join('; ') }}</template>
            </footer>
          </article>

          <div v-for="a in pending" :key="a.approval" class="approval" role="alert">
            <strong>Misty wants to use <span class="mono">{{ a.tool }}</span></strong>
            <span class="pill pending">{{ a.class }}</span>
            <pre class="mono args">{{ JSON.stringify(a.args, null, 2) }}</pre>
            <div class="row">
              <button class="btn primary" type="button" @click="answer(a, true)">Allow</button>
              <button class="btn danger" type="button" @click="answer(a, false)">Refuse</button>
            </div>
          </div>

          <p v-if="!history.length && !live" class="muted">
            Ask a question, tell her to read a file, or ask about your indexed documents. Everything runs on this machine.
          </p>
        </div>

        <form class="card compose" @submit.prevent="send">
          <textarea v-model="input" rows="3" placeholder="Ask Misty (Ctrl+Enter sends)" aria-label="message" :disabled="busy" @keydown.ctrl.enter.prevent="send"></textarea>
          <div class="row">
            <label class="field inline">model
              <select v-model="role" :disabled="busy">
                <option value="">chosen by the router</option>
                <option value="fast">fast</option>
                <option value="main">main</option>
                <option value="deep">deep</option>
              </select>
            </label>
            <label class="field inline">documents
              <select v-model="rag" :disabled="busy">
                <option value="auto">router decides</option>
                <option value="on">always search</option>
                <option value="off">never search</option>
              </select>
            </label>
            <label class="row small"><input v-model="noTools" type="checkbox" :disabled="busy" /> no tools</label>
            <span class="grow"></span>
            <button v-if="busy" class="btn danger" type="button" @click="stop">Stop</button>
            <button class="btn primary" type="submit" :disabled="busy || !input.trim()">Send</button>
          </div>
        </form>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { fold, follow, misty, type Approval, type Ask, type Folded, type MistyStatus, type Session } from '@/misty'

const status = ref<MistyStatus | null>(null)
const sessions = ref<Session[]>([])
const session = ref('')
const history = ref<{ role: string; content: string }[]>([])
const input = ref('')
const role = ref('')
const rag = ref<'auto' | 'on' | 'off'>('auto')
const noTools = ref(false)
const busy = ref(false)
const runId = ref('')
const live = ref<Folded | null>(null)
const pending = ref<Approval[]>([])
const error = ref('')
const scroller = ref<HTMLElement | null>(null)
let stopped = false
let gone = false

const fail = (e: any) => {
  error.value = e?.message || String(e)
}
const toBottom = () => nextTick(() => scroller.value && (scroller.value.scrollTop = scroller.value.scrollHeight))

async function refresh() {
  try {
    sessions.value = await misty.sessions()
  } catch (e) {
    fail(e)
  }
}

async function open(id: string) {
  try {
    history.value = await misty.session(id)
    session.value = id
    live.value = null
    pending.value = []
    error.value = ''
    toBottom()
  } catch (e) {
    fail(e)
  }
}

function fresh() {
  session.value = ''
  history.value = []
  live.value = null
  pending.value = []
  error.value = ''
}

async function drop(id: string) {
  try {
    await misty.remove(id)
    if (id === session.value) fresh()
    await refresh()
  } catch (e) {
    fail(e)
  }
}

async function send() {
  const text = input.value.trim()
  if (!text || busy.value) return
  busy.value = true
  stopped = false
  error.value = ''
  history.value.push({ role: 'user', content: text })
  input.value = ''
  live.value = fold([])
  toBottom()
  try {
    const a: Ask = { input: text, approvals: 'ask', ...(session.value ? { session: session.value } : { new_session: true }) }
    if (role.value) a.role = role.value as Ask['role']
    if (rag.value !== 'auto') a.rag = rag.value === 'on'
    if (noTools.value) a.no_tools = true
    const r = await misty.start(a)
    runId.value = r.run
    if (r.session) session.value = r.session
    await follow(
      r.run,
      (p, all) => {
        live.value = fold(all)
        pending.value = p.pending
        toBottom()
      },
      () => stopped || gone,
    )
    pending.value = []
    if (!gone && live.value && live.value.text && live.value.status !== 'error') {
      // the stored history is the record: show it, with this run's footer kept under the last answer
      const last = live.value
      history.value.push({ role: 'assistant', content: last.text })
      live.value = { ...last, text: '', reasoning: '', notes: [], tools: [] }
    }
  } catch (e) {
    fail(e)
  } finally {
    busy.value = false
    runId.value = ''
    refresh()
    toBottom()
  }
}

async function answer(a: Approval, allow: boolean) {
  try {
    await misty.approve(a.approval, allow)
    pending.value = pending.value.filter((x) => x.approval !== a.approval)
  } catch (e) {
    fail(e)
  }
}

async function stop() {
  stopped = true
  if (runId.value) {
    try {
      await misty.cancel(runId.value)
    } catch (e) {
      fail(e)
    }
  }
}

onMounted(async () => {
  try {
    status.value = await misty.status()
  } catch (e) {
    fail(e)
  }
  refresh()
})
onBeforeUnmount(() => {
  gone = true
  if (runId.value) misty.cancel(runId.value).catch(() => {})
})
</script>

<style scoped>
.layout { display: grid; grid-template-columns: 240px minmax(0, 1fr); gap: 14px; align-items: start; }
.sessions ul { list-style: none; padding: 0; margin: 8px 0 0; }
.sessions li { display: flex; align-items: center; gap: 4px; border-radius: 6px; }
.sessions li.on { background: var(--panel-2); }
.link { flex: 1 1 auto; text-align: left; background: none; border: 0; padding: 6px 8px; color: var(--text); display: flex; flex-direction: column; min-width: 0; }
.mini { background: none; border: 0; color: var(--muted); padding: 2px 6px; }
.chat { min-width: 0; }
.thread { min-height: 320px; max-height: calc(100vh - 340px); overflow-y: auto; }
.msg { margin-bottom: 14px; }
.msg > strong { margin-right: 8px; }
.msg.user .text { background: var(--panel-2); border-radius: 8px; padding: 8px 12px; margin-top: 4px; }
.text { white-space: pre-wrap; word-break: break-word; margin-top: 4px; }
.notes { list-style: none; padding: 0; margin: 4px 0; }
.cites { margin: 6px 0 0 18px; }
.approval { border: 1px solid var(--warn); border-radius: 8px; padding: 10px 12px; margin-bottom: 14px; }
.args { white-space: pre-wrap; max-height: 200px; overflow: auto; margin: 6px 0; }
.compose textarea { margin-bottom: 8px; }
.field.inline { flex-direction: row; align-items: center; gap: 6px; }
@media (max-width: 760px) {
  .layout { grid-template-columns: 1fr; }
  .thread { max-height: none; }
}
</style>

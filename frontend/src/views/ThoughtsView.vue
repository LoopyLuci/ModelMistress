<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Thoughts</h2>
      <span class="pill" :class="link === 'live' ? 'ok' : link === 'down' ? 'error' : 'running'">
        {{ link === 'live' ? 'live' : link === 'down' ? 'reconnecting' : 'connecting' }}
      </span>
      <label class="row muted"><input v-model="follow" type="checkbox" /> follow new requests</label>
    </div>

    <p v-if="error" class="error-box">{{ error }}</p>

    <details v-if="canSend" class="card">
      <summary>Ask a model</summary>
      <div class="row ask">
        <select v-model="model" aria-label="model">
          <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
        </select>
        <span class="grow"></span>
      </div>
      <textarea v-model="prompt" rows="3" placeholder="Type a request. It is traced, and appears below as the model thinks."></textarea>
      <div class="row">
        <button class="btn primary" :disabled="sending || !model || !prompt.trim()" @click="send">Send</button>
        <button v-if="sending" class="btn" @click="abort">Stop</button>
        <span class="muted">Any client can do the same with <span class="mono">"mm_trace": true</span> in its request.</span>
      </div>
    </details>

    <div v-if="recent.length > 1" class="row recent">
      <span class="muted">Recent</span>
      <button v-for="r in recent" :key="r.id" class="btn" :class="{ primary: r.id === trace?.id }" @click="select(r.id)">
        {{ r.model }} <span class="muted-in">{{ r.status === 'running' ? 'running' : '' }}</span>
      </button>
    </div>

    <TraceView v-if="trace" :trace="trace" :tokens="tokens" :live="trace.status === 'running'" :now="clock" />
    <div v-else class="card muted">
      Nothing has been traced yet. Turn tracing on in
      <router-link to="/traces">Traces</router-link>, or send a request here or with <span class="mono">"mm_trace": true</span>;
      the tokens, their probabilities and the model's reasoning will appear here as it runs.
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import TraceView from '@/components/TraceView.vue'
import { call, chatTraced, inTauri, subscribe, type LiveState } from '@/transport'

const trace = ref<any>(null)
const tokens = ref<any[]>([])
const recent = ref<{ id: string; model: string; status: string }[]>([])
const link = ref<LiveState>('connecting')
const follow = ref(true)
const error = ref('')
const clock = ref(Date.now())

const models = ref<string[]>([])
const model = ref('')
const prompt = ref('')
const sending = ref(false)
const canSend = computed(() => !inTauri())
let ctl: AbortController | null = null
let stop: (() => void) | null = null
let timer: number | undefined

async function load(id: string) {
  try {
    const [t, k] = await Promise.all([call('trace.get', { id }), call('trace.tokens', { id, limit: 20000 })])
    if (trace.value && trace.value.id !== id && !(follow.value && t.status === 'running')) return
    trace.value = t
    tokens.value = k.tokens
  } catch (e: any) {
    error.value = e.message
  }
}

async function select(id: string) {
  follow.value = recent.value[0]?.id === id
  await load(id)
}

function remember(id: string, m: string, status: string) {
  const i = recent.value.findIndex((r) => r.id === id)
  if (i >= 0) recent.value[i].status = status
  else recent.value = [{ id, model: m, status }, ...recent.value].slice(0, 8)
}

function onEvent(e: any) {
  if (e.type === 'hello') {
    const running = e.running?.[0]
    if (running && follow.value) load(running.id)
    return
  }
  if (e.type === 'trace.start') {
    remember(e.trace, e.model, 'running')
    if (!follow.value) return
    trace.value = {
      id: e.trace, model: e.model, kind: e.kind, status: 'running', started_ms: e.ts, stages: [], events: [],
      job_id: e.job, task_id: e.task, specialist_calls: [],
    }
    tokens.value = []
    return
  }
  const t = trace.value
  if (!t || e.trace !== t.id) return
  switch (e.type) {
    case 'stage':
      if (e.state === 'start') t.stages.push({ name: e.name, started_ms: e.ts, ended_ms: null })
      else if (e.state === 'end') {
        const open = [...t.stages].reverse().find((s: any) => s.name === e.name && s.ended_ms == null)
        if (open) open.ended_ms = e.ts
      } else t.stages.push({ name: e.name, started_ms: e.start, ended_ms: e.end })
      break
    case 'event':
      t.events.push({ id: t.events.length, kind: e.kind, data: e.data, ts_ms: e.ts })
      break
    case 'tokens':
      if (e.offset === tokens.value.length) tokens.value.push(...e.tokens)
      else if (e.offset > tokens.value.length) load(t.id)
      break
    case 'update':
      Object.assign(t, e.patch)
      break
    case 'trace.finish':
      remember(t.id, t.model, e.status || 'ok')
      load(t.id)
      break
  }
}

async function send() {
  sending.value = true
  error.value = ''
  follow.value = true
  ctl = new AbortController()
  try {
    await chatTraced(model.value, prompt.value, ctl.signal)
  } catch (e: any) {
    if (e.name !== 'AbortError') error.value = e.message
  } finally {
    sending.value = false
  }
}
const abort = () => ctl?.abort()

onMounted(async () => {
  timer = window.setInterval(() => (clock.value = Date.now()), 250)
  stop = subscribe(onEvent, (s) => (link.value = s))
  try {
    const l = await call('trace.list', { limit: 8 })
    recent.value = l.items.map((t: any) => ({ id: t.id, model: t.model, status: t.status }))
    if (l.items[0] && !trace.value) await load(l.items[0].id)
    if (canSend.value) {
      const ms = await call('model.list', {})
      models.value = ms.filter((m: any) => m.source !== 'whisper').map((m: any) => m.id)
      model.value = models.value[0] || ''
    }
  } catch (e: any) {
    error.value = e.message
  }
})

onBeforeUnmount(() => {
  stop?.()
  ctl?.abort()
  window.clearInterval(timer)
})
</script>

<style scoped>
.bar { margin-bottom: 4px; }
.ask { margin-bottom: 8px; }
.recent { margin-bottom: 12px; }
.recent .btn { max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.muted-in { opacity: 0.7; }
</style>

<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Traces</h2>
      <button class="btn" @click="showSettings = !showSettings">Tracing settings</button>
      <button class="btn" @click="doExport">Export JSONL</button>
    </div>
    <p v-if="error" class="error-box">{{ error }}</p>
    <p v-if="note" class="muted">{{ note }}</p>

    <div v-if="showSettings && settings" class="card">
      <h3>Tracing</h3>
      <div class="row">
        <label class="row"><input v-model="settings.enabled" type="checkbox" /> trace every chat request</label>
        <label class="row">alternatives per token <input v-model.number="settings.top_k" type="number" min="1" max="20" style="width: 70px" /></label>
        <label class="row">keep days <input v-model.number="settings.max_age_days" type="number" min="0" style="width: 80px" /></label>
        <label class="row">keep MB <input v-model.number="settings.max_total_mb" type="number" min="0" style="width: 100px" /></label>
        <button class="btn primary" @click="saveSettings">Save</button>
      </div>
      <p class="muted small">
        0 keeps everything. {{ fmtBytes(settings.stored_bytes) }} stored. A single request can ask for a trace with
        <span class="mono">"mm_trace": true</span>.
      </p>
    </div>

    <form class="card filters" @submit.prevent="apply">
      <div class="row">
        <input v-model="f.q" type="text" placeholder="search prompt, answer, reasoning" class="grow" />
        <input v-model="f.model" type="text" placeholder="model" style="width: 130px" />
        <select v-model="f.status" aria-label="status">
          <option value="">any status</option>
          <option v-for="s in ['running', 'ok', 'error', 'aborted']" :key="s" :value="s">{{ s }}</option>
        </select>
        <input v-model="f.task" type="text" placeholder="task id" style="width: 120px" />
        <input v-model="f.job" type="text" placeholder="job id" style="width: 120px" />
        <button class="btn primary">Filter</button>
        <button type="button" class="btn" @click="clear">Clear</button>
      </div>
    </form>

    <div class="row bar" v-if="picked.length">
      <span class="muted">{{ picked.length }} selected</span>
      <button class="btn primary" :disabled="picked.length !== 2" @click="compare">Compare the two</button>
      <button class="btn" @click="picked = []">Clear selection</button>
    </div>

    <div class="list">
      <div v-for="t in items" :key="t.id" class="card item">
        <input v-model="picked" type="checkbox" :value="t.id" :disabled="picked.length >= 2 && !picked.includes(t.id)" aria-label="select to compare" />
        <router-link :to="`/traces/${t.id}`" class="body">
          <div class="row">
            <strong>{{ t.model || t.kind }}</strong>
            <span class="pill" :class="t.status">{{ t.status }}</span>
            <span v-if="t.parent_id" class="pill">rerun</span>
            <span v-if="t.task_id" class="pill">task {{ t.task_id }}</span>
            <span v-if="t.job_id" class="pill">job {{ t.job_id }}</span>
            <span class="grow"></span>
            <span class="muted small">{{ when(t.started_ms) }}</span>
          </div>
          <div class="preview">{{ t.prompt_preview }}</div>
          <div class="preview muted">{{ t.answer_preview }}</div>
          <div class="muted small">
            {{ t.token_count }} tokens<span v-if="t.tokens_per_s"> &middot; {{ Number(t.tokens_per_s).toFixed(1) }} tok/s</span
            ><span v-if="t.duration_ms != null"> &middot; {{ (t.duration_ms / 1000).toFixed(1) }} s</span
            ><span v-if="t.adapters"> &middot; {{ t.adapters }} adapter(s)</span>
          </div>
        </router-link>
      </div>
      <p v-if="!items.length && !loading" class="muted">No traces match.</p>
    </div>

    <div class="row pager">
      <button class="btn" :disabled="offset === 0" @click="page(-1)">Newer</button>
      <span class="muted">{{ total ? `${offset + 1}-${Math.min(offset + size, total)} of ${total}` : '' }}</span>
      <button class="btn" :disabled="offset + size >= total" @click="page(1)">Older</button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { call, exportTraces } from '@/transport'

const route = useRoute()
const router = useRouter()
const q = route.query
const f = reactive({
  q: (q.q as string) || '', model: (q.model as string) || '', status: (q.status as string) || '',
  task: (q.task as string) || '', job: (q.job as string) || '',
})
const items = ref<any[]>([])
const total = ref(0)
const offset = ref(0)
const size = 25
const loading = ref(false)
const error = ref('')
const note = ref('')
const picked = ref<string[]>([])
const settings = ref<any>(null)
const showSettings = ref(false)

const filter = () => Object.fromEntries(Object.entries(f).filter(([, v]) => v !== ''))

async function fetchPage() {
  loading.value = true
  error.value = ''
  try {
    const r = await call('trace.list', { ...filter(), limit: size, offset: offset.value })
    items.value = r.items
    total.value = r.total
  } catch (e: any) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}

const apply = () => {
  offset.value = 0
  router.replace({ query: filter() })
  fetchPage()
}
const clear = () => {
  Object.keys(f).forEach((k) => ((f as any)[k] = ''))
  apply()
}
const page = (d: number) => {
  offset.value = Math.max(0, offset.value + d * size)
  fetchPage()
}
const compare = () => router.push(`/diff/${picked.value[0]}/${picked.value[1]}`)

async function saveSettings() {
  try {
    const { stored_bytes, ...s } = settings.value
    settings.value = await call('trace.settings', s)
    note.value = 'Saved.'
  } catch (e: any) {
    error.value = e.message
  }
}

async function doExport() {
  try {
    note.value = await exportTraces(filter())
  } catch (e: any) {
    error.value = e.message
  }
}

const when = (ms: number) => new Date(ms).toLocaleString()
const fmtBytes = (b: number) => (b > 1e6 ? (b / 1e6).toFixed(1) + ' MB' : Math.round((b || 0) / 1e3) + ' KB')

onMounted(async () => {
  fetchPage()
  try {
    settings.value = await call('trace.settings', {})
  } catch (_) {
    /* shown by the list */
  }
})
</script>

<style scoped>
.bar { margin-bottom: 10px; }
.small { font-size: 0.8rem; }
.item { display: flex; gap: 12px; align-items: flex-start; margin-bottom: 8px; }
.body { flex: 1 1 auto; min-width: 0; color: inherit; text-decoration: none; }
.preview { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.pager { justify-content: center; margin: 12px 0 24px; }
@media (max-width: 640px) {
  .preview { white-space: normal; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
}
</style>

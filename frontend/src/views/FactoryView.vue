<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Factory</h2>
      <router-link to="/factory/review" class="btn">Review queue</router-link>
      <button class="btn primary" :disabled="gone" @click="showForm = !showForm">{{ showForm ? 'Hide form' : 'New task' }}</button>
    </div>
    <Unavailable v-if="gone" what="Factory" op="factory.task.list" />
    <p v-else-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <form v-if="showForm && !gone" class="card" aria-label="new factory task" @submit.prevent="create">
      <h3>New task</h3>
      <label class="field wide">Goal
        <textarea v-model="s.goal" rows="2" required placeholder="Async Rust with tokio: tasks, cancellation, select!, Send bounds"></textarea>
      </label>
      <fieldset class="kinds">
        <legend>KMs to build</legend>
        <label v-for="k in KINDS" :key="k" class="row small"><input v-model="s.kinds" type="checkbox" :value="k" /> {{ k }}</label>
      </fieldset>
      <div class="formgrid">
        <label class="field">Domain path
          <input v-model="s.domain" type="text" placeholder="code/rust/async" required />
        </label>
        <label class="field">Author model
          <input v-model="s.author" type="text" list="models" required />
        </label>
        <label class="field">Reviewer model
          <input v-model="s.reviewer" type="text" list="models" />
        </label>
        <label class="field">Base core
          <select v-model="s.base"><option v-for="b in ['dusty', 'misty', 'any']" :key="b" :value="b">{{ b }}</option></select>
        </label>
        <label class="field">Records (target)
          <input v-model.number="s.size.records" type="number" min="1" />
        </label>
        <label class="field">Max tokens
          <input v-model.number="s.size.max_tokens" type="number" min="1" />
        </label>
        <label class="field">GPU hours (budget)
          <input v-model.number="s.budget.gpu_hours" type="number" min="0" step="0.5" />
        </label>
        <label class="field">Vocabulary
          <input v-model="s.tagging.vocabulary" type="text" />
        </label>
        <label class="field">Tag review
          <select v-model="s.tagging.review"><option v-for="x in ['auto', 'all', 'none']" :key="x" :value="x">{{ x }}</option></select>
        </label>
        <label class="field">Approval of the result
          <select v-model="s.approval"><option v-for="x in ['gated', 'propose', 'auto', 'off']" :key="x" :value="x">{{ x }}</option></select>
        </label>
      </div>
      <datalist id="models"><option v-for="m in models" :key="m" :value="m"></option></datalist>

      <fieldset class="srcs">
        <legend>Sources</legend>
        <div v-for="(src, i) in s.sources" :key="i" class="row src">
          <select v-model="src.kind" :aria-label="`kind of source ${i + 1}`" @change="resetSource(src)">
            <option v-for="k in Object.keys(SRC)" :key="k" :value="k">{{ k }}</option>
          </select>
          <input v-model="src[SRC[src.kind]]" type="text" class="grow" :aria-label="`${SRC[src.kind]} of source ${i + 1}`" :placeholder="SRC[src.kind]" />
          <button type="button" class="btn" :aria-label="`remove source ${i + 1}`" @click="s.sources.splice(i, 1)">Remove</button>
        </div>
        <button type="button" class="btn" @click="s.sources.push({ kind: 'dir', path: '' })">Add source</button>
        <p class="muted small">Folders and files are read on the hub's machine.</p>
      </fieldset>

      <details class="json">
        <summary>The task as JSON</summary>
        <pre class="mono">{{ JSON.stringify(spec(), null, 2) }}</pre>
      </details>
      <div class="row">
        <button class="btn primary" :disabled="busy">Create task</button>
      </div>
    </form>

    <div v-if="!gone" class="card tablewrap">
      <table class="t" aria-label="factory tasks">
        <thead><tr><th>task</th><th>status</th><th>step</th><th>progress</th><th>updated</th></tr></thead>
        <tbody>
          <tr v-for="t in tasks" :key="t.id">
            <td>
              <router-link :to="`/factory/${encodeURIComponent(t.id)}`"><strong>{{ t.goal || t.id }}</strong></router-link>
              <div class="mono muted small">{{ t.id }}</div>
            </td>
            <td><span class="pill" :class="statusClass(t.status)">{{ t.status }}</span></td>
            <td>{{ t.step || '-' }}</td>
            <td>
              <span class="meter" role="progressbar" :aria-valuenow="pct(t)" aria-valuemin="0" aria-valuemax="100"><span :style="{ width: pct(t) + '%' }"></span></span>
              <span class="small"> {{ pct(t) }}%</span>
            </td>
            <td class="muted small">{{ fmtWhen(t.updated_ms || t.created_ms) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="!tasks.length && loaded" class="muted">No factory tasks yet.</p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import Unavailable from '@/components/Unavailable.vue'
import { factory, modelIds, unavailable, type FactorySpec, type FactoryTask } from '@/api'
import { fmtWhen } from '@/util'

const KINDS = ['data', 'memory', 'adapter', 'skill', 'tool']
// The field each kind of source is described by (km-factory.md §2).
const SRC: Record<string, string> = { dir: 'path', url: 'url', km: 'id', skill: 'path', tool: 'name', traces: 'query' }

const tasks = ref<FactoryTask[]>([])
const models = ref<string[]>([])
const gone = ref(false)
const loaded = ref(false)
const error = ref('')
const note = ref('')
const busy = ref(false)
const showForm = ref(false)
const s = reactive({
  goal: '', kinds: ['data', 'memory'] as string[], domain: '', sources: [{ kind: 'dir', path: '' }] as Record<string, string>[],
  author: '', reviewer: '', base: 'dusty', size: { records: 500 as number | undefined, max_tokens: undefined as number | undefined },
  tagging: { vocabulary: 'default', review: 'auto' }, approval: 'gated', budget: { gpu_hours: undefined as number | undefined },
})
let timer = 0

const clean = <T extends Record<string, any>>(o: T): T =>
  Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined && v !== null && v !== '' && !Number.isNaN(v))) as T
function spec(): FactorySpec {
  return {
    goal: s.goal.trim(), kinds: s.kinds, domain: s.domain.trim(),
    sources: s.sources.filter((x) => x[SRC[x.kind]]?.trim()).map((x) => ({ kind: x.kind, [SRC[x.kind]]: x[SRC[x.kind]].trim() })),
    author: s.author.trim(), ...(s.reviewer.trim() ? { reviewer: s.reviewer.trim() } : {}), base: s.base, size: clean(s.size), tagging: { ...s.tagging },
    approval: s.approval, budget: clean(s.budget),
  }
}
const resetSource = (src: Record<string, string>) => {
  for (const k of Object.keys(src)) if (k !== 'kind') delete src[k]
  src[SRC[src.kind]] = ''
}
const pct = (t: FactoryTask) => {
  const p = Number(t.progress) || 0 // 0..1, or already a percentage
  return Math.round(Math.max(0, Math.min(100, p > 1 ? p : p * 100)))
}
const statusClass = (st: string) => (['done', 'complete', 'completed', 'published'].includes(st) ? 'done' : ['failed', 'error'].includes(st) ? 'failed' : st === 'cancelled' ? 'cancelled' : ['running', 'planning'].includes(st) ? 'running' : st === 'awaiting_approval' ? 'pending' : '')

async function load() {
  try {
    tasks.value = await factory.list()
    error.value = ''
    loaded.value = true
  } catch (e: any) {
    if (unavailable(e)) gone.value = true
    else error.value = e.message
  }
  if (!gone.value) timer = window.setTimeout(load, 3000)
}

async function create() {
  busy.value = true
  error.value = ''
  try {
    const r = await factory.create(spec())
    note.value = `Created task ${r?.id || r?.task || ''}.`
    showForm.value = false
    await load()
  } catch (e: any) {
    if (unavailable(e)) gone.value = true
    else error.value = e.message
  } finally {
    busy.value = false
  }
}

onMounted(async () => {
  modelIds().then((m) => (models.value = m)).catch(() => {})
  await load()
})
onBeforeUnmount(() => window.clearTimeout(timer))
</script>

<style scoped>
.wide { margin-bottom: 10px; }
.kinds, .srcs { border: 1px solid var(--border); border-radius: 8px; padding: 8px 12px; margin-bottom: 10px; display: flex; flex-wrap: wrap; gap: 6px 16px; }
.srcs { display: block; }
.kinds legend, .srcs legend { padding: 0 6px; color: var(--muted); font-size: 0.85rem; }
.src { margin-bottom: 6px; flex-wrap: nowrap; }
.json { margin: 10px 0; }
.json pre { overflow-x: auto; padding: 8px; background: var(--panel-2); border-radius: 8px; }
.meter { display: inline-block; width: 90px; height: 8px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 4px; vertical-align: middle; overflow: hidden; }
.meter > span { display: block; height: 100%; background: var(--accent); }
</style>

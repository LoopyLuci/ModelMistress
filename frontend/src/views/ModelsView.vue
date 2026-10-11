<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Models</h2>
      <button class="btn" type="button" :disabled="busy" @click="reload(true)">Rescan disk</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <div class="tabs" role="tablist">
      <button v-for="t in tabs" :key="t.id" role="tab" :aria-selected="tab === t.id" @click="tab = t.id">{{ t.label }}<span v-if="t.n" class="pill"> {{ t.n }}</span></button>
    </div>

    <!-- Installed ------------------------------------------------------------------------------------------------ -->
    <template v-if="tab === 'installed'">
      <section v-if="loadedList.length" class="card" aria-label="Loaded models">
        <h3>Loaded now</h3>
        <div v-for="l in loadedList" :key="l.model" class="loadedrow">
          <div class="row">
            <strong>{{ l.model }}</strong>
            <span v-if="l.rss_mb != null" class="pill" title="memory held by the model server process">{{ gb(l.rss_mb) }} in RAM</span>
            <span v-for="b in l.buffers" :key="b.what" class="pill" title="buffer llama.cpp reports">{{ b.what }} {{ gb(b.mib) }}</span>
            <span class="pill">{{ where(l) }}</span>
            <span class="muted small">up {{ dur(l.uptime_s) }}, idle {{ dur(l.idle_s) }}</span>
            <span class="grow"></span>
            <button class="btn" type="button" :disabled="busy" @click="unload(l.model)">Unload</button>
          </div>
        </div>
      </section>

      <p v-if="!models.length && ready" class="muted">No models found. Add a folder in Settings, or download one under Find and download.</p>

      <section v-for="g in groups" :key="g.label" class="group" :aria-label="g.label">
        <h3 class="gh">{{ g.label }}</h3>
        <div v-for="m in g.models" :key="m.id" class="card model" :class="{ on: m.loaded }">
          <div class="row">
            <strong class="name">{{ m.id }}</strong>
            <span v-if="m.native" class="pill good">MM model{{ m.family ? ': ' + m.family : '' }}</span>
            <span v-if="m.loaded" class="pill running">loaded</span>
            <span v-if="m.vision" class="pill">vision</span>
            <span v-if="m.experts" class="pill" title="mixture of experts">{{ m.experts }} experts{{ m.experts_used ? ', top ' + m.experts_used : '' }}</span>
            <span class="grow"></span>
            <button v-if="!m.loaded" class="btn primary" type="button" :disabled="busy" @click="openLoad(m)">Load...</button>
            <button v-else class="btn" type="button" :disabled="busy" @click="unload(m.id)">Unload</button>
            <router-link v-if="isChatModel(m)" class="btn" :to="{ path: '/', query: { model: m.id } }">Chat</router-link>
            <button class="btn" type="button" @click="toggleInfo(m)">{{ info[m.id] ? 'Hide details' : 'Details' }}</button>
            <button class="btn danger" type="button" :disabled="busy" @click="askDelete = m.id">Delete</button>
          </div>
          <dl class="kv facts">
            <dt>Size</dt><dd>{{ m.size_gb != null ? m.size_gb + ' GB' : '-' }}</dd>
            <dt>Quantisation</dt><dd>{{ m.file_type || '-' }}</dd>
            <dt>Architecture</dt><dd>{{ m.architecture || '-' }}{{ m.size_label ? ' (' + m.size_label + ')' : '' }}</dd>
            <dt>Context</dt><dd>{{ m.context_length ? m.context_length.toLocaleString() + ' tokens' : '-' }}</dd>
            <dt>Runs on</dt><dd>{{ m.source === 'ollama-daemon' ? 'Ollama' : 'llama.cpp (built in)' }}{{ m.loaded ? ', loaded' : '' }}</dd>
          </dl>

          <form v-if="loadFor === m.id" class="loadform" @submit.prevent="doLoad(m)">
            <div class="formgrid">
              <label class="field">Context size (tokens, blank = setting)
                <input v-model.number="opts.ctx_size" type="number" min="256" step="256" placeholder="setting" />
              </label>
              <label class="field">GPU layers (blank = setting, 999 = all)
                <input v-model.number="opts.gpu_layers" type="number" min="0" placeholder="setting" />
              </label>
              <template v-if="m.experts">
                <label class="field">Experts on the CPU
                  <select v-model="opts.cpu_moe">
                    <option value="default">{{ m.native ? 'Yes (default for MM models)' : 'No (default)' }}</option>
                    <option value="yes">Yes: all experts in system memory, the rest on the GPU</option>
                    <option value="no">No: everything on the GPU</option>
                  </select>
                </label>
                <label class="field">Only the first N layers' experts on the CPU
                  <input v-model.number="opts.n_cpu_moe" type="number" min="0" placeholder="all" />
                </label>
              </template>
            </div>
            <div class="row">
              <button class="btn primary" type="submit" :disabled="busy">{{ loading === m.id ? 'Loading...' : 'Load' }}</button>
              <button class="btn" type="button" :disabled="loading === m.id" @click="loadFor = ''">Cancel</button>
              <span v-if="loading === m.id" class="muted small">A large model takes a minute or more to load.</span>
            </div>
          </form>

          <div v-if="info[m.id]" class="details">
            <dl class="kv small">
              <dt>File</dt><dd class="mono">{{ info[m.id].path }}</dd>
              <dt>Projector</dt><dd class="mono">{{ info[m.id].mmproj || '-' }}</dd>
              <template v-for="(v, k) in shownMeta(info[m.id].metadata)" :key="k"><dt class="mono">{{ k }}</dt><dd class="mono">{{ v }}</dd></template>
            </dl>
          </div>
        </div>
      </section>
    </template>

    <!-- Find and download ---------------------------------------------------------------------------------------- -->
    <template v-if="tab === 'find'">
      <form class="card" @submit.prevent="search">
        <h3>Search Hugging Face</h3>
        <div class="row">
          <input v-model="query" type="text" class="grow" placeholder="for example qwen3 8b, gemma, llama" aria-label="Search" />
          <button class="btn primary" :disabled="!query.trim() || searching">{{ searching ? 'Searching...' : 'Search' }}</button>
        </div>
      </form>
      <div v-if="hits.length" class="card">
        <h3>Results, most downloaded first</h3>
        <div v-for="h in hits" :key="h.repo" class="hit">
          <div class="row">
            <strong class="grow name">{{ h.repo }}</strong>
            <span class="muted small">{{ (h.downloads || 0).toLocaleString() }} downloads, {{ h.likes || 0 }} likes</span>
            <button class="btn" type="button" @click="showFiles(h.repo)">{{ files[h.repo] ? 'Hide files' : 'Files' }}</button>
          </div>
          <div v-if="files[h.repo]" class="tablewrap">
            <table class="t">
              <thead><tr><th>File</th><th>Quant</th><th class="num">Size</th><th></th></tr></thead>
              <tbody>
                <tr v-for="f in files[h.repo]" :key="f.file">
                  <td class="mono">{{ f.file }}</td><td>{{ f.quant || '-' }}</td><td class="num">{{ f.size_gb }} GB</td>
                  <td><button v-if="f.pull" class="btn" type="button" :disabled="busy" @click="pull({ model: f.pull, file: f.file })">Download</button><span v-else class="muted small">projector (comes with the model)</span></td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
      <p v-else-if="searched" class="muted">No GGUF models matched.</p>

      <form class="card" @submit.prevent="pullByName">
        <h3>Download by name</h3>
        <div class="row">
          <input v-model="byName" type="text" class="grow" placeholder="hf.co/org/repo:Q4_K_M, or an Ollama name such as llama3.2:1b" aria-label="Model name" />
          <button class="btn primary" :disabled="!byName.trim() || busy">Download</button>
        </div>
        <p class="muted small">Downloads go to the download folder in Settings and resume if stopped.</p>
      </form>
    </template>

    <!-- Downloads ------------------------------------------------------------------------------------------------ -->
    <template v-if="tab === 'downloads'">
      <p v-if="!jobs.length" class="muted">No downloads yet.</p>
      <div v-for="j in jobs" :key="j.job" class="card">
        <div class="row">
          <strong class="grow name">{{ j.model }}</strong>
          <span class="pill" :class="j.status === 'done' ? 'good' : j.status === 'failed' ? 'bad' : j.status">{{ j.status }}</span>
          <button v-if="j.status === 'running'" class="btn" type="button" @click="cancelJob(j.job)">Stop</button>
        </div>
        <progress v-if="j.status === 'running'" :value="j.percent" max="100" class="prog" :aria-label="`Download progress ${j.percent}%`"></progress>
        <p class="muted small">
          <template v-if="j.status === 'running'">{{ j.percent }}% of {{ fmtBytes(j.total_bytes) }}, {{ j.mb_per_s }} MB/s. </template>
          {{ j.message }}
        </p>
      </div>
    </template>

    <ConfirmDialog :open="!!askDelete" title="Delete this model?" action="Delete" @confirm="doDelete" @cancel="askDelete = ''">
      {{ askDelete }} is removed from the disk. Only models ModelMistress downloaded can be deleted; the hub refuses the others.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { call } from '@/transport'
import { fmtBytes } from '@/util'
import { groupModels, isChatModel, listModels, loadedModels, type HubModel, type LoadedModel } from '@/chat/models'

const models = ref<HubModel[]>([])
const loadedList = ref<(LoadedModel & { rss_mb?: number | null; buffers?: { what: string; mib: number }[] })[]>([])
const jobs = ref<any[]>([])
const ready = ref(false)
const busy = ref(false)
const error = ref('')
const note = ref('')
const tab = ref<'installed' | 'find' | 'downloads'>('installed')
const loadFor = ref('')
const loading = ref('')
const opts = reactive<{ ctx_size: number | null; gpu_layers: number | null; cpu_moe: string; n_cpu_moe: number | null }>({
  ctx_size: null, gpu_layers: null, cpu_moe: 'default', n_cpu_moe: null,
})
const info = reactive<Record<string, any>>({})
const askDelete = ref('')
const query = ref('')
const searching = ref(false)
const searched = ref(false)
const hits = ref<any[]>([])
const files = reactive<Record<string, any[]>>({})
const byName = ref('')
let timer: number | undefined

const running = computed(() => jobs.value.filter((j) => j.status === 'running').length)
const tabs = computed(() => [
  { id: 'installed' as const, label: 'Installed', n: models.value.length },
  { id: 'find' as const, label: 'Find and download', n: 0 },
  { id: 'downloads' as const, label: 'Downloads', n: running.value },
])
// Chat models in groups; the rest (whisper, vision nets, image models) after them.
const groups = computed(() => {
  const g = groupModels(models.value.filter(isChatModel))
  const other = models.value.filter((m) => !isChatModel(m))
  return other.length ? [...g, { label: 'Speech, vision and image models', models: other }] : g
})

const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`)
const dur = (s: number) => (s < 90 ? `${s} s` : s < 5400 ? `${Math.round(s / 60)} min` : `${(s / 3600).toFixed(1)} h`)
function where(l: LoadedModel): string {
  const a = l.args || []
  const arg = (k: string) => {
    const i = a.indexOf(k)
    return i >= 0 ? a[i + 1] : undefined
  }
  const ngl = Number(arg('-ngl'))
  const bits = [ngl === 0 ? 'CPU only' : ngl >= 999 ? 'all layers on GPU' : `${ngl} GPU layers`]
  if (a.includes('--cpu-moe')) bits.push('experts on CPU')
  else if (a.includes('--n-cpu-moe')) bits.push(`experts of ${arg('--n-cpu-moe')} layers on CPU`)
  bits.push(`context ${arg('-c') || '?'}`)
  return bits.join(', ')
}
function shownMeta(m: Record<string, any> | undefined) {
  const out: Record<string, any> = {}
  for (const [k, v] of Object.entries(m || {})) if (typeof v !== 'object') out[k] = String(v).slice(0, 120)
  return out
}

async function reload(rescan = false) {
  try {
    const [m, l] = await Promise.all([listModels(rescan), loadedModels()])
    models.value = m
    loadedList.value = l as any
    ready.value = true
  } catch (e: any) {
    error.value = String(e?.message || e)
  }
}
async function pollJobs() {
  try {
    jobs.value = ((await call('job.list', {})) as any[]).slice().sort((a, b) => (b.started || 0) - (a.started || 0))
  } catch (_) {
    /* the hub is gone for a moment */
  }
}
let doneSeen = new Set<string>()
async function tick() {
  await pollJobs()
  for (const j of jobs.value) {
    if (j.status === 'done' && !doneSeen.has(j.job)) {
      doneSeen.add(j.job)
      await reload(true) // a download finished while this page was open
    }
  }
  if (!busy.value) {
    try {
      loadedList.value = (await loadedModels()) as any
    } catch (_) {
      /* ignore */
    }
  }
}

function openLoad(m: HubModel) {
  loadFor.value = loadFor.value === m.id ? '' : m.id
  Object.assign(opts, { ctx_size: null, gpu_layers: null, cpu_moe: 'default', n_cpu_moe: null })
}
async function doLoad(m: HubModel) {
  const a: Record<string, unknown> = { model: m.id }
  if (opts.ctx_size) a.ctx_size = opts.ctx_size
  if (typeof opts.gpu_layers === 'number') a.gpu_layers = opts.gpu_layers
  if (m.experts) {
    if (opts.cpu_moe !== 'default') a.cpu_moe = opts.cpu_moe === 'yes'
    if (opts.n_cpu_moe) a.n_cpu_moe = opts.n_cpu_moe
  }
  error.value = note.value = ''
  busy.value = true
  loading.value = m.id
  try {
    const r = await call('model.load', a)
    note.value = `${m.id} loaded in ${(r.load_ms / 1000).toFixed(1)} s.`
    loadFor.value = ''
  } catch (e: any) {
    error.value = `Could not load ${m.id}: ${e?.message || e}`
  } finally {
    busy.value = false
    loading.value = ''
    await reload()
  }
}
async function unload(id: string) {
  busy.value = true
  error.value = note.value = ''
  try {
    await call('model.unload', { model: id })
    note.value = `${id} unloaded.`
  } catch (e: any) {
    error.value = String(e?.message || e)
  } finally {
    busy.value = false
    await reload()
  }
}
async function toggleInfo(m: HubModel) {
  if (info[m.id]) {
    delete info[m.id]
    return
  }
  try {
    info[m.id] = await call('model.info', { model: m.id })
  } catch (e: any) {
    error.value = String(e?.message || e)
  }
}
async function doDelete() {
  const id = askDelete.value
  askDelete.value = ''
  error.value = note.value = ''
  try {
    await call('model.delete', { model: id })
    note.value = `${id} deleted.`
  } catch (e: any) {
    error.value = String(e?.message || e)
  }
  await reload(true)
}

async function search() {
  searching.value = true
  error.value = ''
  try {
    hits.value = (await call('model.search', { query: query.value.trim(), limit: 25 })) as any[]
    searched.value = true
  } catch (e: any) {
    error.value = `Search failed: ${e?.message || e}`
  } finally {
    searching.value = false
  }
}
async function showFiles(repo: string) {
  if (files[repo]) {
    delete files[repo]
    return
  }
  try {
    files[repo] = (await call('model.files', { model: repo })) as any[]
  } catch (e: any) {
    error.value = String(e?.message || e)
  }
}
async function pull(a: Record<string, unknown>) {
  error.value = note.value = ''
  busy.value = true
  try {
    const r = await call('model.pull', a)
    note.value = `Downloading ${r.model}; progress is under Downloads.`
    tab.value = 'downloads'
    await pollJobs()
  } catch (e: any) {
    error.value = `Could not start the download: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}
function pullByName() {
  const n = byName.value.trim()
  // org/name and hf.co/... are Hugging Face; a plain name is the Ollama library's
  pull({ model: n, ...(n.startsWith('hf.co/') || n.includes('/') ? {} : { source: 'ollama' }) })
}
async function cancelJob(id: string) {
  try {
    await call('job.cancel', { job: id })
  } catch (e: any) {
    error.value = String(e?.message || e)
  }
  await pollJobs()
}

onMounted(async () => {
  await Promise.all([reload(), pollJobs()])
  doneSeen = new Set(jobs.value.filter((j) => j.status === 'done').map((j) => j.job))
  timer = window.setInterval(tick, 2500)
})
onBeforeUnmount(() => window.clearInterval(timer))
</script>

<style scoped>
.gh { font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; color: var(--muted); margin: 14px 0 8px; }
.model.on { border-color: var(--good); }
.name { overflow-wrap: anywhere; }
.facts { margin-top: 8px; }
.loadform { margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--border); }
.details { margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--border); max-height: 320px; overflow: auto; }
.details dd { overflow-wrap: anywhere; }
.loadedrow + .loadedrow { margin-top: 8px; }
.hit { padding: 8px 0; border-bottom: 1px solid var(--border); }
.prog { width: 100%; margin-top: 8px; }
</style>

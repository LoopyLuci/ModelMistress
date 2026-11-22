<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Misty: setup and status</h2>
      <span v-if="status" class="pill" :class="status.ready ? 'ok' : 'warn'" role="status">{{ status.ready ? 'ready' : 'needs setup' }}</span>
      <router-link class="btn" to="/misty">Back to chat</router-link>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <div class="tabs" role="tablist">
      <button v-for="t in TABS" :key="t.id" role="tab" :aria-selected="tab === t.id" type="button" @click="tab = t.id">{{ t.label }}</button>
    </div>

    <!-- STATUS -->
    <div v-if="tab === 'status' && status">
      <ul v-if="status.next.length" class="card next">
        <li v-for="(n, i) in status.next" :key="i">{{ n }}</li>
      </ul>
      <div class="card">
        <h3>Who does what</h3>
        <div class="tablewrap">
          <table class="t" aria-label="roster">
            <thead><tr><th>role</th><th>model</th><th>state</th><th class="num">size</th><th>why</th></tr></thead>
            <tbody>
              <tr v-for="r in status.roster" :key="r.role">
                <td>{{ ROLE_LABEL[r.role] || r.role }}</td>
                <td class="mono">{{ r.model || '-' }}</td>
                <td>
                  <span class="pill" :class="r.status === 'ready' ? 'ok' : 'warn'">{{ r.status }}</span>
                  <span v-if="r.loaded" class="pill ok">loaded</span>
                </td>
                <td class="num">{{ fmtBytes(r.size_bytes) }}</td>
                <td class="muted small">{{ r.source }}<template v-if="r.note"> &middot; {{ r.note }}</template></td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
      <div class="split">
        <div class="card">
          <h3>Engines and memory</h3>
          <dl class="kv">
            <dt>llama.cpp</dt><dd>{{ status.engines.llama_version || 'not found' }}</dd>
            <dt>runs ternary Q2_0</dt><dd>{{ status.engines.stock_q2 ? 'yes (stock llama.cpp is new enough)' : 'no' }}</dd>
            <dt>Prism engine</dt><dd>{{ status.engines.prism ? 'installed' : 'not installed' }}</dd>
            <dt>Cactus runtime</dt><dd>{{ status.cactus.available ? status.cactus.library : 'not installed' }}</dd>
            <dt>free memory</dt><dd>{{ status.free_ram_mb }} MB</dd>
            <dt>runs now</dt><dd>{{ status.runs_active }} running, {{ status.approvals_pending }} waiting for you</dd>
          </dl>
          <p v-if="status.cactus.error" class="error-box small">{{ status.cactus.error }}</p>
          <div class="row">
            <button class="btn" type="button" :disabled="busy" @click="runDoctor">Run the checks</button>
            <button class="btn" type="button" :disabled="busy" @click="runBench">Time each model</button>
          </div>
        </div>
        <div class="card">
          <h3>Policy</h3>
          <dl class="kv">
            <template v-for="(v, k) in status.policy" :key="k"><dt>{{ k }}</dt><dd>{{ v }}</dd></template>
          </dl>
          <p class="muted small">
            auto = runs at once, ask = waits for your answer, deny = never. Reading is automatic; writing waits for you; commands and
            the network are off. Change one with <span class="mono">model-mistress misty policy write auto</span>; other settings with <span class="mono">misty config set</span>.
          </p>
          <h3>Folders she may read and write</h3>
          <ul class="plain"><li v-for="r in status.roots" :key="r" class="mono">{{ r }}</li></ul>
        </div>
      </div>
      <div v-if="report" class="card">
        <h3>{{ report.title }}</h3>
        <ul v-if="report.checks" class="plain checks">
          <li v-for="(c, i) in report.checks" :key="i">
            <span class="pill" :class="c.ok === true ? 'ok' : c.ok === false ? 'bad' : ''">{{ c.ok === true ? 'ok' : c.ok === false ? 'fail' : 'skipped' }}</span>
            <strong class="mono">{{ c.check }}</strong> {{ c.detail }}
            <div v-if="c.ok === false && c.fix" class="muted small">Fix: {{ c.fix }}</div>
          </li>
        </ul>
        <pre v-else class="mono out">{{ report.raw }}</pre>
      </div>
    </div>

    <!-- SETUP -->
    <div v-if="tab === 'setup' && plan">
      <div class="card">
        <h3>What can be installed</h3>
        <p class="muted small">
          Every download is checked against its size and SHA-256. The default set is {{ fmtBytes(plan.default_download_bytes) }} still to download.
        </p>
        <div class="tablewrap">
          <table class="t" aria-label="components">
            <thead><tr><th></th><th>component</th><th>what it is</th><th class="num">size</th><th>state</th></tr></thead>
            <tbody>
              <tr v-for="c in plan.components" :key="c.id">
                <td><input v-model="picked" type="checkbox" :value="c.id" :disabled="c.installed || busy" :aria-label="`install ${c.id}`" /></td>
                <td class="mono">{{ c.id }} <span v-if="c.default" class="pill">default</span></td>
                <td>{{ c.what }}<div v-if="c.note" class="muted small">{{ c.note }}</div></td>
                <td class="num">{{ fmtBytes(c.size_bytes) }}</td>
                <td><span class="pill" :class="c.installed ? 'ok' : ''">{{ c.installed ? 'installed' : 'not installed' }}</span></td>
              </tr>
            </tbody>
          </table>
        </div>
        <div class="row">
          <button class="btn primary" type="button" :disabled="busy" @click="install(['default'])">Install the default set</button>
          <button class="btn" type="button" :disabled="busy || !picked.length" @click="install(picked)">Install the {{ picked.length }} ticked</button>
        </div>
      </div>
      <div v-if="myJobs.length" class="card">
        <h3>Downloads</h3>
        <div v-for="j in myJobs" :key="j.job" class="job">
          <div class="row">
            <strong class="grow">{{ j.model }}</strong>
            <span class="pill" :class="j.status === 'done' ? 'good' : j.status === 'failed' ? 'bad' : j.status">{{ j.status }}</span>
          </div>
          <progress v-if="j.status === 'running'" :value="j.percent" max="100" class="prog" :aria-label="`${j.model} ${j.percent}%`"></progress>
          <div class="muted small">
            <template v-if="j.status === 'running'">{{ j.percent }}% of {{ fmtBytes(j.total_bytes) }}. </template>{{ j.message }}
          </div>
        </div>
      </div>
    </div>

    <!-- DOCUMENTS -->
    <div v-if="tab === 'docs'">
      <div class="card">
        <h3>Add to the index</h3>
        <form class="row" @submit.prevent="ingest">
          <input v-model="ingestPath" type="text" class="grow" placeholder="A file or folder on this machine, for example D:\notes" aria-label="path to index" />
          <button class="btn primary" type="submit" :disabled="busy || !ingestPath.trim()">Index it</button>
        </form>
        <p class="muted small">Unchanged files are skipped, and files that look like secrets are never indexed. It reads only inside the folders listed under Status.</p>
      </div>
      <div class="card">
        <h3>Search the index</h3>
        <form class="row" @submit.prevent="search">
          <input v-model="query" type="text" class="grow" placeholder="Search your documents" aria-label="search the index" />
          <button class="btn" type="submit" :disabled="busy || !query.trim()">Search</button>
        </form>
        <ol v-if="hits.length" class="hits">
          <li v-for="h in hits" :key="h.chunk">
            <div class="mono small">{{ h.source }}:{{ h.start_line }}-{{ h.end_line }} <span class="pill">{{ h.via }}</span> <span class="muted">{{ h.score.toFixed(3) }}</span></div>
            <div class="snippet">{{ h.text }}</div>
          </li>
        </ol>
      </div>
      <div class="card">
        <h3>Indexed sources</h3>
        <div v-for="c in sources.collections" :key="c.collection" class="muted small">{{ c.collection }}: {{ c.sources }} sources, {{ c.chunks }} chunks</div>
        <div class="tablewrap">
          <table v-if="sources.sources.length" class="t" aria-label="sources">
            <thead><tr><th>collection</th><th>source</th><th class="num">size</th><th class="num">chunks</th><th class="num">vectors</th><th></th></tr></thead>
            <tbody>
              <tr v-for="s in sources.sources" :key="s.collection + s.source">
                <td>{{ s.collection }}</td>
                <td class="mono">{{ s.source }}</td>
                <td class="num">{{ fmtBytes(s.bytes) }}</td>
                <td class="num">{{ s.chunks }}</td>
                <td class="num">{{ s.vectors }}</td>
                <td><button class="btn danger" type="button" :disabled="busy" @click="unindex(s)">Remove</button></td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="!sources.sources.length" class="muted">Nothing is indexed yet.</p>
      </div>
    </div>

    <!-- TOOLS -->
    <div v-if="tab === 'tools' && tools">
      <div class="card">
        <h3>What she can do</h3>
        <div class="tablewrap">
          <table class="t" aria-label="tools">
            <thead><tr><th>tool</th><th>class</th><th>policy</th><th>what it does</th></tr></thead>
            <tbody>
              <tr v-for="t in tools.tools" :key="t.name">
                <td class="mono">{{ t.name }}</td>
                <td>{{ t.class }}</td>
                <td><span class="pill" :class="t.policy === 'auto' ? 'ok' : t.policy === 'deny' ? 'bad' : 'pending'">{{ t.policy }}</span></td>
                <td class="small">{{ t.description }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
      <div v-if="pipelines.length" class="card">
        <h3>Pipelines</h3>
        <ul class="plain">
          <li v-for="p in pipelines" :key="p.name"><strong class="mono">{{ p.name }}</strong> <span class="muted small">{{ p.description }} ({{ p.steps?.length ?? 0 }} steps)</span></li>
        </ul>
        <p class="muted small">Run one with <span class="mono">model-mistress misty pipeline run NAME --input key=value</span>.</p>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { call } from '@/transport'
import { fmtBytes } from '@/util'
import { misty, ROLE_LABEL, type Hit, type MistyStatus, type Pipeline, type SetupPlan, type Source, type ToolInfo } from '@/misty'

const TABS = [
  { id: 'status', label: 'Status' },
  { id: 'setup', label: 'Setup' },
  { id: 'docs', label: 'Documents' },
  { id: 'tools', label: 'Tools' },
]
const tab = ref('status')
const status = ref<MistyStatus | null>(null)
const plan = ref<SetupPlan | null>(null)
const picked = ref<string[]>([])
const jobs = ref<any[]>([])
const sources = ref<{ sources: Source[]; collections: { collection: string; sources: number; chunks: number }[] }>({ sources: [], collections: [] })
const tools = ref<{ tools: ToolInfo[]; policy: Record<string, string> } | null>(null)
const pipelines = ref<Pipeline[]>([])
const hits = ref<Hit[]>([])
const query = ref('')
const ingestPath = ref('')
const report = ref<{ title: string; checks?: any[]; raw?: string } | null>(null)
const error = ref('')
const note = ref('')
const busy = ref(false)
let timer = 0

// downloads this page started carry the "misty" source; Bonsai files go through the ordinary download path
const myJobs = computed(() => jobs.value.filter((j) => j.source === 'misty' || /bonsai/i.test(String(j.model))).slice(0, 12))

async function guard<T>(f: () => Promise<T>): Promise<T | undefined> {
  busy.value = true
  error.value = ''
  note.value = ''
  try {
    return await f()
  } catch (e: any) {
    error.value = e?.message || String(e)
  } finally {
    busy.value = false
  }
}

async function load() {
  try {
    if (tab.value === 'status') status.value = await misty.status()
    if (tab.value === 'setup') plan.value = await misty.setupPlan()
    if (tab.value === 'docs') sources.value = await misty.sources()
    if (tab.value === 'tools') {
      tools.value = await misty.tools()
      pipelines.value = await misty.pipelines()
    }
    if (!status.value) status.value = await misty.status()
  } catch (e: any) {
    error.value = e?.message || String(e)
  }
}

async function pollJobs() {
  try {
    jobs.value = ((await call('job.list', {})) as any[]).slice().sort((a, b) => (b.started || 0) - (a.started || 0))
  } catch (_) {
    /* the hub is gone for a moment */
  }
}

async function runDoctor() {
  const r: any = await guard(() => misty.doctor())
  if (r) report.value = { title: 'Checks', checks: Array.isArray(r.checks) ? r.checks : undefined, raw: JSON.stringify(r, null, 2) }
}

async function runBench() {
  const r: any = await guard(() => misty.bench(64))
  if (r) report.value = { title: 'Timing', raw: JSON.stringify(r, null, 2) }
}

async function install(ids: string[]) {
  const r: any = await guard(() => misty.setupRun(ids))
  if (r) {
    note.value = `Started ${r.jobs?.length ?? 0} download(s)${r.skipped?.length ? `, ${r.skipped.length} already installed` : ''}.`
    picked.value = []
    await pollJobs()
    plan.value = await misty.setupPlan()
  }
}

async function ingest() {
  const r: any = await guard(() => misty.ingest(ingestPath.value.trim()))
  if (r) {
    note.value = `Indexed: ${JSON.stringify(r)}`
    sources.value = await misty.sources()
  }
}

async function search() {
  const r = await guard(() => misty.search(query.value.trim()))
  if (r) hits.value = r
}

async function unindex(s: Source) {
  if (!window.confirm(`Remove ${s.source} from the index? The file itself is not touched.`)) return
  await guard(() => misty.removeSource(s.source, s.collection))
  sources.value = await misty.sources()
}

watch(tab, load)
onMounted(() => {
  load()
  pollJobs()
  timer = window.setInterval(() => {
    pollJobs()
    if (tab.value === 'setup' && myJobs.value.some((j) => j.status === 'running')) load()
  }, 2500)
})
onBeforeUnmount(() => window.clearInterval(timer))
</script>

<style scoped>
.next { list-style: none; padding: 10px 14px; border-color: var(--warn); }
.plain { list-style: none; padding: 0; margin: 0; }
.checks li { margin-bottom: 6px; }
.out { white-space: pre-wrap; max-height: 360px; overflow: auto; }
.hits { padding-left: 20px; }
.hits li { margin-bottom: 10px; }
.snippet { white-space: pre-wrap; word-break: break-word; font-size: 0.9rem; max-height: 140px; overflow: auto; }
.job { margin-bottom: 10px; }
.prog { width: 100%; height: 8px; }
@media (max-width: 760px) {
  .split { grid-template-columns: 1fr; }
}
</style>

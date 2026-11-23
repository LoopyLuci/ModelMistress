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
          <p v-if="l.serving?.placement_note" class="muted small">{{ l.serving.placement_note }}</p>
        </div>
      </section>

      <p v-if="!models.length && ready" class="muted">No models found. Add a folder in Settings, or download one under Find and download.</p>

      <div v-if="models.length" class="row filters" role="search">
        <input v-model="filter" type="search" class="grow" placeholder="Filter by name, family or quantisation" aria-label="Filter models" />
        <select v-model="show" aria-label="Show">
          <option value="all">All models</option>
          <option value="loaded">Loaded</option>
          <option value="roles">With a router role</option>
          <option value="custom">With custom settings</option>
          <option value="mine">Downloaded here (can be deleted)</option>
        </select>
        <select v-model="sort" aria-label="Sort by">
          <option value="name">Name</option>
          <option value="size">Largest first</option>
          <option value="loaded">Loaded first</option>
        </select>
      </div>
      <p v-if="models.length && !shownCount" class="muted">No model matches the filter.</p>

      <section v-for="g in groups" :key="g.label" class="group" :aria-label="g.label">
        <h3 class="gh">{{ g.label }}</h3>
        <div v-for="m in g.models" :key="m.id" class="card model" :class="{ on: m.loaded }">
          <div class="row">
            <strong class="name">{{ m.id }}</strong>
            <span v-if="m.native" class="pill good">MM model{{ m.family ? ': ' + m.family : '' }}</span>
            <span v-if="m.loaded" class="pill running">loaded</span>
            <span v-if="m.quant" class="pill" title="the quantisation of this file">{{ m.quant }}</span>
            <span
              v-if="m.hardware"
              class="pill"
              :class="{ good: m.hardware.runs_on === 'gpu', bad: m.hardware.runs_on === 'none' }"
              :title="hardwareTitle(m.hardware)"
            >{{ m.hardware.now ? 'on ' : '' }}{{ m.hardware.chip }}</span>
            <span v-if="m.vision" class="pill">vision</span>
            <span v-if="m.experts" class="pill" title="mixture of experts">{{ m.experts }} experts{{ m.experts_used ? ', top ' + m.experts_used : '' }}</span>
            <span v-for="r in rolesOf(m.id)" :key="r.role" class="pill good" :title="roleTitle(r)">router: {{ r.role }}</span>
            <span v-if="isCustom(profiles[m.id])" class="pill" :title="describe(profiles[m.id])">custom settings</span>
            <span class="grow"></span>
            <button v-if="!m.loaded" class="btn primary" type="button" :disabled="busy" @click="openLoad(m)">Load...</button>
            <button v-else class="btn" type="button" :disabled="busy" @click="unload(m.id)">Unload</button>
            <router-link v-if="isChatModel(m)" class="btn" :to="{ path: '/', query: { model: m.id } }">Chat</router-link>
            <button class="btn" type="button" @click="toggleEdit(m)">{{ editFor === m.id ? 'Close settings' : 'Settings' }}</button>
            <button class="btn" type="button" @click="toggleInfo(m)">{{ info[m.id] ? 'Hide details' : 'Details' }}</button>
            <button
              class="btn danger"
              type="button"
              :disabled="busy || m.deletable === false"
              :title="m.deletable === false ? 'ModelMistress did not download this file, so it does not delete it. Remove it yourself, or delete it in the program that owns it.' : 'Remove the model from the disk'"
              @click="askDelete = m.id"
            >Delete</button>
          </div>
          <dl class="kv facts">
            <dt>Size</dt><dd>{{ m.size_gb != null ? m.size_gb + ' GB' : '-' }}</dd>
            <dt>Quantisation</dt><dd>{{ m.file_type || '-' }}</dd>
            <dt>Architecture</dt><dd>{{ m.architecture || '-' }}{{ m.size_label ? ' (' + m.size_label + ')' : '' }}</dd>
            <dt>Context</dt><dd>{{ m.context_length ? m.context_length.toLocaleString() + ' tokens' : '-' }}</dd>
            <dt>Runs on</dt><dd>{{ m.source === 'ollama-daemon' ? 'Ollama' : (m.hardware?.engine || 'llama.cpp') + ' (built in)' }}{{ m.loaded ? ', loaded' : '' }}</dd>
            <template v-if="m.hardware"><dt>Hardware</dt><dd>{{ hardwareTitle(m.hardware) }}</dd></template>
            <template v-if="isCustom(profiles[m.id])"><dt>Settings</dt><dd>{{ describe(profiles[m.id]) }}</dd></template>
          </dl>

          <div v-if="roleChoices(m).length && router" class="row userole">
            <label class="muted small" :for="`use-${m.id}`">Use for</label>
            <select :id="`use-${m.id}`" :value="''" :disabled="busy" @change="useFor(m, ($event.target as HTMLSelectElement))">
              <option value="">Choose a router role...</option>
              <option v-for="r in roleChoices(m)" :key="r" :value="r">{{ r }}: {{ ROLE_HELP[r] }}</option>
            </select>
          </div>

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
            <label v-if="learnedCpu" class="small"><input v-model="opts.retry_gpu" type="checkbox" /> Try the GPU again (it crashed the last time)</label>
            <p v-if="fit" class="fit" :class="fit.tone" role="status">{{ fit.text }}</p>
            <p v-else-if="fitNote" class="muted small">{{ fitNote }}</p>
            <div class="row">
              <button class="btn primary" type="submit" :disabled="busy">{{ loading === m.id ? 'Loading...' : 'Load' }}</button>
              <button class="btn" type="button" :disabled="loading === m.id" @click="loadFor = ''">Cancel</button>
              <span v-if="loading === m.id" class="muted small">A large model takes a minute or more to load.</span>
            </div>
          </form>

          <form v-if="editFor === m.id" class="loadform" @submit.prevent="saveSettings(m, false)">
            <h4>Settings for this model</h4>
            <p class="muted small">
              These are used every time {{ m.id }} loads, whoever asks (a chat, the router, another program). A blank field
              uses the global setting from the Settings page.
            </p>
            <div class="formgrid">
              <label v-for="f in FIELDS" :key="f.key" class="field">{{ f.label }}
                <select v-if="f.kind === 'select'" v-model="form[f.key]">
                  <option v-for="o in f.options" :key="o" :value="o">{{ o || 'setting' }}</option>
                </select>
                <select v-else-if="f.kind === 'tri'" v-model="form[f.key]">
                  <option value="">setting</option>
                  <option value="true">on</option>
                  <option value="false">off</option>
                </select>
                <input v-else v-model="form[f.key]" type="text" inputmode="numeric" placeholder="setting" />
                <span class="muted small">{{ f.help }}</span>
              </label>
            </div>
            <p v-if="editError" class="error-box" role="alert">{{ editError }}</p>
            <div class="row">
              <button class="btn primary" type="submit" :disabled="busy">Save</button>
              <button v-if="m.loaded" class="btn" type="button" :disabled="busy" @click="saveSettings(m, true)">Save and reload the model</button>
              <button class="btn" type="button" :disabled="busy || !isCustom(profiles[m.id])" @click="resetSettings(m)">Reset to the global settings</button>
              <button class="btn" type="button" @click="editFor = ''">Close</button>
            </div>
            <p v-if="m.loaded" class="muted small">The model is loaded: a saved change applies the next time it loads.</p>
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

    <!-- Router --------------------------------------------------------------------------------------------------- -->
    <template v-if="tab === 'router'">
      <p v-if="!router" class="muted">
        {{ routerError || 'The model router is not available on this hub (it comes with Misty).' }}
      </p>
      <template v-else>
        <section class="card" aria-label="Router roles">
          <h3>Which model answers</h3>
          <p class="muted">
            Choose the model <code>auto</code> in any chat, or in any program that speaks the OpenAI API, and the router sends each
            message to <strong>fast</strong>, <strong>main</strong> or <strong>deep</strong> by how demanding it is. A model that is
            already loaded is preferred. Leave a role on Automatic and the router picks an installed model of the right size that
            fits the free memory.
            <router-link :to="{ path: '/', query: { model: 'auto' } }">Chat with Auto</router-link>
          </p>
          <p class="muted small">Free memory the router counts on: {{ gb(router.free_ram_mb) }}.</p>
          <div class="tablewrap">
            <table class="t roles">
              <thead><tr><th>Role</th><th>Model</th><th>State</th><th></th></tr></thead>
              <tbody>
                <tr v-for="r in roleRows" :key="r.role">
                  <td>
                    <strong>{{ r.role }}</strong>
                    <div class="muted small">{{ ROLE_HELP[r.role] }}</div>
                  </td>
                  <td>
                    <select :value="router.overrides[r.role] || ''" :disabled="busy" :aria-label="`Model for ${r.role}`" @change="chooseRole(r.role, ($event.target as HTMLSelectElement).value)">
                      <option value="">Automatic{{ router.overrides[r.role] ? '' : r.model ? ': ' + r.model : '' }}</option>
                      <option v-for="m in modelsFor(r.role)" :key="m.id" :value="m.id">{{ m.id }}{{ m.size_gb ? ` (${m.size_gb} GB)` : '' }}{{ m.loaded ? ' - loaded' : '' }}</option>
                    </select>
                    <div v-if="router.overrides[r.role] && !modelsFor(r.role).some((m) => m.id === router!.overrides[r.role])" class="muted small">
                      {{ router.overrides[r.role] }} is chosen but is not installed.
                    </div>
                  </td>
                  <td>
                    <span class="pill" :class="r.status === 'ready' ? 'good' : 'warn'">{{ STATUS_TEXT[r.status] || r.status }}</span>
                    <span v-if="r.loaded" class="pill running">loaded</span>
                    <div class="muted small">{{ SOURCE_LABEL[r.source] || r.source }}</div>
                    <div v-if="r.note" class="muted small">{{ r.note }}</div>
                  </td>
                  <td class="num">
                    <button v-if="r.model && !r.loaded" class="btn" type="button" :disabled="busy || r.status === 'missing'" @click="loadRole(r.model)">Load now</button>
                    <button v-else-if="r.model && r.loaded" class="btn" type="button" :disabled="busy" @click="unload(r.model)">Unload</button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>

        <form class="card" aria-label="Try the router" @submit.prevent="tryRouter">
          <h3>Try the router</h3>
          <p class="muted small">Type a message to see which model it would go to. Nothing is sent to a model. Start a message with <code>@fast</code>, <code>@main</code> or <code>@deep</code> to choose the role yourself.</p>
          <div class="row">
            <input v-model="tryText" type="text" class="grow" placeholder="for example: what is the capital of France?" aria-label="Message to test" />
            <label class="small"><input v-model="tryImage" type="checkbox" /> with a picture</label>
            <button class="btn primary" :disabled="!tryText.trim() || trying">{{ trying ? 'Checking...' : 'Check' }}</button>
          </div>
          <div v-if="tried" class="tryout">
            <p v-if="tried.model">
              Goes to <strong>{{ tried.model }}</strong> as <span class="pill good">{{ tried.role }}</span>
              <span v-if="tried.forced" class="muted small"> (you asked for {{ tried.forced }})</span>
            </p>
            <p v-else class="error-box">No model is ready for this message.</p>
            <ul class="small"><li v-for="(w, i) in tried.reasons" :key="i">{{ w }}</li></ul>
            <div class="tablewrap">
              <table class="t">
                <thead><tr><th>Role</th><th>Model</th><th>State</th><th class="num">Score</th></tr></thead>
                <tbody>
                  <tr v-for="c in tried.candidates" :key="c.role" :class="{ chosen: c.role === tried.role }">
                    <td>{{ c.role }}</td>
                    <td>{{ c.model || '-' }}</td>
                    <td>{{ STATUS_TEXT[c.status] || c.status }}{{ c.loaded ? ', loaded' : '' }}{{ c.note ? ': ' + c.note : '' }}</td>
                    <td class="num">{{ c.score != null ? c.score.toFixed(2) : '-' }}</td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        </form>
      </template>
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
      {{ askDelete }} is removed from the disk and cannot be brought back except by downloading it again.
      <template v-if="rolesOf(askDelete).length"> It is the router's model for {{ rolesOf(askDelete).map((r) => r.role).join(', ') }}; the router will pick another.</template>
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { call } from '@/transport'
import { fmtBytes } from '@/util'
import { groupModels, isChatModel, listModels, loadedModels, type HubModel, type LoadedModel } from '@/chat/models'
import { BRAIN_ROLES, ROLE_HELP, SOURCE_LABEL, STATUS_TEXT, pickText, routerStatus, setRole, suitsRole, type Pick, type RoleView, type RouterStatus } from '@/chat/router'
import { FIELDS, describe, isCustom, readProfiles, saveProfile, toForm, toProfile, type Form, type Profile } from '@/chat/profiles'

type TabId = 'installed' | 'router' | 'find' | 'downloads'

const route = useRoute()
const models = ref<HubModel[]>([])
const loadedList = ref<(LoadedModel & { rss_mb?: number | null; buffers?: { what: string; mib: number }[] })[]>([])
const jobs = ref<any[]>([])
const ready = ref(false)
const busy = ref(false)
const error = ref('')
const note = ref('')
const wanted = String(route.query.tab || '')
const tab = ref<TabId>((['installed', 'router', 'find', 'downloads'] as string[]).includes(wanted) ? (wanted as TabId) : 'installed')
const loadFor = ref('')
const loading = ref('')
const opts = reactive<{ ctx_size: number | null; gpu_layers: number | null; cpu_moe: string; n_cpu_moe: number | null; retry_gpu: boolean }>({
  ctx_size: null, gpu_layers: null, cpu_moe: 'default', n_cpu_moe: null, retry_gpu: false,
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

// the installed list: filter and order
const filter = ref('')
const show = ref<'all' | 'loaded' | 'roles' | 'custom' | 'mine'>('all')
const sort = ref<'name' | 'size' | 'loaded'>('name')

// the router and the per-model settings
const router = ref<RouterStatus | null>(null)
const routerError = ref('')
const profiles = ref<Record<string, Profile>>({})
const editFor = ref('')
const editError = ref('')
const form = reactive<Form>({})
const tryText = ref('')
const tryImage = ref(false)
const trying = ref(false)
const tried = ref<Pick | null>(null)

// would the load fit?
const fit = ref<{ text: string; tone: string } | null>(null)
const fitNote = ref('')
const learnedCpu = ref('') // why the hub starts the model being loaded on the CPU (it crashed the GPU before)
let fitTimer: number | undefined
let fitSeq = 0

const running = computed(() => jobs.value.filter((j) => j.status === 'running').length)
const tabs = computed(() => [
  { id: 'installed' as const, label: 'Installed', n: models.value.length },
  { id: 'router' as const, label: 'Router', n: 0 },
  { id: 'find' as const, label: 'Find and download', n: 0 },
  { id: 'downloads' as const, label: 'Downloads', n: running.value },
])

/** The router's roles that name this model: fast, main, deep and the retrieval models (embed, rerank). */
function rolesOf(id: string): RoleView[] {
  const r = router.value
  if (!r || !id) return []
  return [...r.roles, ...(r.support || [])].filter((x) => x.model === id)
}
const roleTitle = (r: RoleView) => `${r.role}: ${ROLE_HELP[r.role] || ''} (${SOURCE_LABEL[r.source] || r.source})`

const wantsRole = (m: HubModel, name: string) => rolesOf(m.id).some((r) => r.role === name)
const matches = (m: HubModel): boolean => {
  const q = filter.value.trim().toLowerCase()
  if (q && ![m.id, m.family, m.architecture, m.file_type, m.name].some((v) => (v || '').toLowerCase().includes(q))) return false
  switch (show.value) {
    case 'loaded': return !!m.loaded
    case 'roles': return rolesOf(m.id).length > 0
    case 'custom': return isCustom(profiles.value[m.id])
    case 'mine': return m.deletable !== false
    default: return true
  }
}
const order = (a: HubModel, b: HubModel): number => {
  if (sort.value === 'size') return (b.size_gb || 0) - (a.size_gb || 0) || a.id.localeCompare(b.id)
  if (sort.value === 'loaded') return Number(!!b.loaded) - Number(!!a.loaded) || a.id.localeCompare(b.id)
  return a.id.localeCompare(b.id)
}

// Chat models in groups; embedders and rerankers, then the rest (whisper, vision nets, image models), after them.
const groups = computed(() => {
  const all = models.value.filter(matches)
  const chat = all.filter(isChatModel)
  const other = all.filter((m) => !isChatModel(m))
  const support = other.filter((m) => suitsRole('embed', m.id, false) || suitsRole('rerank', m.id, false))
  const rest = other.filter((m) => !support.includes(m))
  const g = [
    ...groupModels(chat),
    ...(support.length ? [{ label: 'Embedding and reranking models', models: support }] : []),
    ...(rest.length ? [{ label: 'Speech, vision and image models', models: rest }] : []),
  ]
  return g.map((x) => ({ ...x, models: x.models.slice().sort(order) }))
})
const shownCount = computed(() => groups.value.reduce((n, g) => n + g.models.length, 0))

const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`)
const dur = (s: number) => (s < 90 ? `${s} s` : s < 5400 ? `${Math.round(s / 60)} min` : `${(s / 3600).toFixed(1)} h`)
function where(l: LoadedModel): string {
  const a = l.args || []
  const arg = (k: string) => {
    const i = a.indexOf(k)
    return i >= 0 ? a[i + 1] : undefined
  }
  // where the weights went, as measured; else as the arguments say (llama-server chose them itself under --fit)
  const ngl = arg('-ngl') === undefined ? NaN : Number(arg('-ngl'))
  const placed = l.serving?.placement
  const bits = [
    placed === 'cpu' || ngl === 0 ? 'CPU only'
      : placed === 'partial' ? 'partly on the GPU'
      : placed === 'gpu' ? 'on the GPU'
      : Number.isNaN(ngl) ? 'GPU layers chosen by llama-server'
      : ngl >= 999 ? 'all layers on GPU' : `${ngl} GPU layers`,
  ]
  if (a.includes('--cpu-moe')) bits.push('experts on CPU')
  else if (a.includes('--n-cpu-moe')) bits.push(`experts of ${arg('--n-cpu-moe')} layers on CPU`)
  bits.push(`context ${arg('-c') || '?'}`)
  return bits.join(', ')
}
/** What the hardware chip means, in words: where it runs or would go, what it could run on, and the memory it takes. */
function hardwareTitle(h: NonNullable<HubModel['hardware']>): string {
  const mib = (v?: number | null) => (v == null ? '' : v >= 1024 ? `${(v / 1024).toFixed(1)} GB` : `${v} MB`)
  const where: Record<string, string> = {
    gpu: 'on the GPU',
    split: h.gpu_share != null ? `${Math.round(h.gpu_share * 100)}% on the GPU, the rest from RAM` : 'split between the GPU and RAM',
    cpu: 'on the CPU',
    none: 'does not fit this machine as asked',
    unknown: 'not measured',
  }
  const bits = [(h.now ? 'Running ' : 'Would load ') + (where[h.runs_on] || h.runs_on)]
  if (h.vram_mib) bits.push(`${mib(h.vram_mib)} of video memory`)
  if (h.ram_mib) bits.push(`${mib(h.ram_mib)} of RAM`)
  bits.push(h.can_run_on.length > 1 ? 'can run on GPU or CPU' : 'CPU only (this engine build has no GPU support here)')
  return bits.join('; ')
}
function shownMeta(m: Record<string, any> | undefined) {
  const out: Record<string, any> = {}
  for (const [k, v] of Object.entries(m || {})) if (typeof v !== 'object') out[k] = String(v).slice(0, 120)
  return out
}

async function loadRouter() {
  try {
    router.value = await routerStatus()
    routerError.value = ''
  } catch (e: any) {
    router.value = null
    routerError.value = /no operation/i.test(String(e?.message || e)) ? '' : `The router is not available: ${e?.message || e}`
  }
}
async function loadProfiles() {
  try {
    profiles.value = (await readProfiles()).profiles
  } catch (_) {
    /* the settings are not readable now: no badges */
  }
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
  await Promise.all([loadRouter(), loadProfiles()])
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

// ---- load ----------------------------------------------------------------------------------------------------------
function loadArgs(m: HubModel): Record<string, unknown> {
  const a: Record<string, unknown> = { model: m.id }
  if (opts.ctx_size) a.ctx_size = opts.ctx_size
  if (typeof opts.gpu_layers === 'number') a.gpu_layers = opts.gpu_layers
  if (opts.retry_gpu) a.retry_gpu = true
  if (m.experts) {
    if (opts.cpu_moe !== 'default') a.cpu_moe = opts.cpu_moe === 'yes'
    if (opts.n_cpu_moe) a.n_cpu_moe = opts.n_cpu_moe
  }
  return a
}
function openLoad(m: HubModel) {
  loadFor.value = loadFor.value === m.id ? '' : m.id
  Object.assign(opts, { ctx_size: null, gpu_layers: null, cpu_moe: 'default', n_cpu_moe: null, retry_gpu: false })
  fit.value = null
  fitNote.value = ''
  learnedCpu.value = ''
}
async function checkFit(m: HubModel) {
  const seq = ++fitSeq
  try {
    const r = await call('model.plan', loadArgs(m))
    if (seq !== fitSeq) return
    fitNote.value = ''
    learnedCpu.value = typeof r.learned_cpu === 'string' ? r.learned_cpu : ''
    fit.value = describeFit(r)
  } catch (e: any) {
    if (seq !== fitSeq) return
    fit.value = null
    fitNote.value = `No fit estimate: ${e?.message || e}`
  }
}
function describeFit(r: any): { text: string; tone: string } | null {
  const need = r.estimate?.total_mib as number | undefined
  const free = r.free_mib as number | undefined
  const sizes = need != null && free != null ? ` It needs about ${gb(need)}; ${gb(free)} of video memory is free.` : ''
  const p = r.placement as { where?: string; gpu_share?: number; need_ram_mib?: number; free_ram_mib?: number } | null | undefined
  // layers the hub and llama-server choose (the default): what does not fit on the GPU runs from RAM
  if (r.learned_cpu) return { tone: 'warn', text: `It starts on the CPU: ${r.learned_cpu}. Tick "Try the GPU again" to give the GPU another go.` }
  if (p?.where === 'split') return { tone: 'warn', text: `About ${Math.round((p.gpu_share ?? 0) * 100)}% of it goes on the GPU and the rest runs from system memory, which is slower.${sizes}` }
  if (p?.where === 'cpu') return { tone: 'warn', text: `Next to no video memory is free, so it runs on the CPU.${sizes}` }
  if (p?.where === 'none') return { tone: 'bad', text: `This does not fit this machine: it needs about ${gb(p.need_ram_mib ?? 0)} of system memory and ${gb(p.free_ram_mib ?? 0)} are free. A smaller context, fewer parallel requests or a smaller quantisation helps.${sizes}` }
  switch (r.verdict) {
    case 'fits': return { tone: 'ok', text: `This fits.${sizes}` }
    case 'fits_after_unloading': return { tone: 'warn', text: `This fits after unloading ${(r.unload || []).join(', ') || 'another model'}; the hub does that when you load.${sizes}` }
    case 'does_not_fit': return { tone: 'bad', text: `This does not fit.${sizes}${r.reclaimable_mib ? ` Unloading everything else frees ${gb(r.reclaimable_mib)}.` : ''} A smaller context or fewer GPU layers helps.` }
    default: return { tone: 'muted', text: 'The hub cannot estimate this one (no GPU found, or an architecture it does not know). It can still be loaded.' }
  }
}
watch(
  () => [loadFor.value, opts.ctx_size, opts.gpu_layers, opts.cpu_moe, opts.n_cpu_moe],
  () => {
    window.clearTimeout(fitTimer)
    const m = models.value.find((x) => x.id === loadFor.value)
    if (!m) return
    fitTimer = window.setTimeout(() => checkFit(m), 350) // the owner is still typing
  },
)

async function doLoad(m: HubModel) {
  error.value = note.value = ''
  busy.value = true
  loading.value = m.id
  try {
    const r = await call('model.load', loadArgs(m))
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
async function loadRole(id: string) {
  error.value = note.value = ''
  busy.value = true
  try {
    const r = await call('model.load', { model: id })
    note.value = `${id} loaded in ${(r.load_ms / 1000).toFixed(1)} s.`
  } catch (e: any) {
    error.value = `Could not load ${id}: ${e?.message || e}`
  } finally {
    busy.value = false
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

// ---- roles ---------------------------------------------------------------------------------------------------------
/** Rows of the roles table: the three brains, then what retrieval uses. */
const roleRows = computed<RoleView[]>(() => {
  const r = router.value
  if (!r) return []
  const by = (name: string) => [...r.roles, ...(r.support || [])].find((x) => x.role === name)
  const blank = (role: string): RoleView => ({ role, model: null, status: 'missing', source: 'fallback', loaded: false, note: '' })
  return [...BRAIN_ROLES, 'embed', 'rerank'].map((n) => by(n) || blank(n))
})
/** The installed models that may fill a role. */
const modelsFor = (role: string): HubModel[] => models.value.filter((m) => suitsRole(role, m.id, isChatModel(m))).sort((a, b) => a.id.localeCompare(b.id))
/** The roles a model could be given from its card. */
function roleChoices(m: HubModel): string[] {
  return [...BRAIN_ROLES, 'embed', 'rerank'].filter((r) => suitsRole(r, m.id, isChatModel(m)) && !wantsRole(m, r) && !(router.value?.overrides[r] === m.id))
}
async function chooseRole(role: string, id: string) {
  busy.value = true
  error.value = note.value = ''
  try {
    await setRole(role, id || null)
    note.value = id ? `${id} is now the model for ${role}.` : `${role} goes back to the router's own choice.`
  } catch (e: any) {
    error.value = `Could not change ${role}: ${e?.message || e}`
  } finally {
    busy.value = false
    await loadRouter()
  }
}
async function useFor(m: HubModel, el: HTMLSelectElement) {
  const role = el.value
  el.value = ''
  if (role) await chooseRole(role, m.id)
}
async function tryRouter() {
  trying.value = true
  error.value = ''
  try {
    tried.value = await pickText(tryText.value.trim(), tryImage.value)
  } catch (e: any) {
    tried.value = null
    error.value = `The router did not answer: ${e?.message || e}`
  } finally {
    trying.value = false
  }
}

// ---- settings of one model -----------------------------------------------------------------------------------------
function toggleEdit(m: HubModel) {
  if (editFor.value === m.id) {
    editFor.value = ''
    return
  }
  editFor.value = m.id
  editError.value = ''
  for (const k of Object.keys(form)) delete form[k]
  Object.assign(form, toForm(profiles.value[m.id]))
}
async function saveSettings(m: HubModel, reloadAfter: boolean) {
  editError.value = ''
  error.value = note.value = ''
  busy.value = true
  try {
    const saved = await saveProfile(m.id, toProfile(form, profiles.value[m.id]))
    profiles.value = saved.profiles
    note.value = `Settings for ${m.id} saved.`
    if (reloadAfter && m.loaded) {
      await call('model.unload', { model: m.id })
      const r = await call('model.load', { model: m.id })
      note.value = `Settings for ${m.id} saved; it was reloaded in ${(r.load_ms / 1000).toFixed(1)} s.`
    } else if (m.loaded) {
      note.value += ' They apply the next time it loads.'
    }
  } catch (e: any) {
    editError.value = `Not saved: ${e?.message || e}`
  } finally {
    busy.value = false
    await reload()
  }
}
async function resetSettings(m: HubModel) {
  busy.value = true
  editError.value = ''
  try {
    profiles.value = (await saveProfile(m.id, {})).profiles
    Object.assign(form, toForm(undefined))
    note.value = `${m.id} uses the global settings again.`
  } catch (e: any) {
    editError.value = `Not reset: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

// ---- find and download ---------------------------------------------------------------------------------------------
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

// a link such as /models?tab=router while this page is open
watch(
  () => route.query.tab,
  (t) => {
    if (typeof t === 'string' && (['installed', 'router', 'find', 'downloads'] as string[]).includes(t)) tab.value = t as TabId
  },
)

onMounted(async () => {
  await Promise.all([reload(), pollJobs()])
  doneSeen = new Set(jobs.value.filter((j) => j.status === 'done').map((j) => j.job))
  timer = window.setInterval(tick, 2500)
})
onBeforeUnmount(() => {
  window.clearInterval(timer)
  window.clearTimeout(fitTimer)
})
</script>

<style scoped>
.gh { font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; color: var(--muted); margin: 14px 0 8px; }
.model.on { border-color: var(--good); }
.name { overflow-wrap: anywhere; }
.facts { margin-top: 8px; }
.filters { margin: 10px 0; gap: 8px; }
.userole { margin-top: 8px; gap: 8px; }
.loadform { margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--border); }
.loadform h4 { margin: 0 0 4px; }
.fit { margin: 8px 0; padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); }
.fit.ok { border-color: var(--good); }
.fit.warn { border-color: var(--warn, var(--border)); }
.fit.bad { border-color: var(--bad); }
.details { margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--border); max-height: 320px; overflow: auto; }
.details dd { overflow-wrap: anywhere; }
.loadedrow + .loadedrow { margin-top: 8px; }
.hit { padding: 8px 0; border-bottom: 1px solid var(--border); }
.prog { width: 100%; margin-top: 8px; }
.roles td { vertical-align: top; }
.tryout { margin-top: 10px; }
.tryout ul { margin: 4px 0 8px 1.2em; }
tr.chosen td { background: var(--panel-2); }
</style>

<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Settings</h2>
      <button class="btn" type="button" :disabled="!dirty || saving" @click="revert">Revert</button>
      <button class="btn primary" type="button" :disabled="!dirty || saving" @click="save">{{ saving ? 'Saving...' : 'Save' }}</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>
    <p v-if="!form && !error" class="muted">Loading...</p>

    <template v-if="form">
      <section v-for="g in sections" :key="g.title" class="card">
        <h3>{{ g.title }}</h3>
        <p v-if="g.help" class="muted small gh">{{ g.help }}</p>
        <div class="fields">
          <label v-for="f in g.fields" :key="f.key" class="field" :class="{ wide: f.kind === 'lines' || f.kind === 'path' || f.kind === 'text' }">
            <span>{{ f.label }}<span v-if="changed(f.key)" class="pill warn"> changed</span></span>
            <input v-if="f.kind === 'bool'" v-model="form[f.key]" type="checkbox" class="chk" />
            <input v-else-if="f.kind === 'int'" v-model.number="form[f.key]" type="number" :min="f.min ?? 0" />
            <textarea v-else-if="f.kind === 'lines'" v-model="form[f.key]" rows="3" spellcheck="false" :placeholder="f.placeholder || 'one per line'"></textarea>
            <select v-else-if="f.kind === 'model'" v-model="form[f.key]">
              <option value="">(none)</option>
              <option v-for="m in modelIds" :key="m" :value="m">{{ m }}</option>
            </select>
            <input v-else-if="f.kind === 'secret'" v-model="form[f.key]" type="password" autocomplete="off" />
            <input v-else v-model="form[f.key]" type="text" spellcheck="false" :placeholder="f.placeholder || ''" />
            <span v-if="f.help" class="muted small">{{ f.help }}</span>
          </label>
        </div>
      </section>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { call } from '@/transport'
import { isChatModel, listModels } from '@/chat/models'

type Kind = 'path' | 'lines' | 'text' | 'int' | 'bool' | 'model' | 'secret'
interface Field { key: string; label: string; kind: Kind; help?: string; placeholder?: string; min?: number; nullable?: boolean }

// What config.get returns is shown here as a form. Known settings get a label, a kind and a help line; any other
// plain setting the hub adds later still shows up (under "More"), typed from its value.
const GROUPS: { title: string; help?: string; fields: Field[] }[] = [
  {
    title: 'Model folders',
    help: 'Where models are found and where downloads go. Changes apply to the next rescan.',
    fields: [
      { key: 'model_dirs', label: 'Folders of .gguf files', kind: 'lines', help: 'Searched four levels deep.' },
      { key: 'ollama_discovery', label: 'Use Ollama models', kind: 'bool', help: 'Off: no Ollama store is scanned and ollama/<name> is refused.' },
      { key: 'ollama_dirs', label: 'Ollama model stores', kind: 'lines', help: 'The folder that holds manifests and blobs.' },
      { key: 'hf_dirs', label: 'Hugging Face caches', kind: 'lines' },
      { key: 'download_dir', label: 'Download folder', kind: 'path', nullable: true, help: 'Where downloads are saved (empty: the hub home folder).' },
      { key: 'hf_token', label: 'Hugging Face token', kind: 'secret', nullable: true, help: 'For gated or private repositories. Shown as (set) when there is one.' },
    ],
  },
  {
    title: 'Loading and memory',
    fields: [
      { key: 'max_loaded', label: 'Most models loaded at once', kind: 'int', min: 1 },
      { key: 'idle_unload_s', label: 'Unload after idle seconds', kind: 'int', help: '0 keeps models loaded.' },
      { key: 'ctx_size', label: 'Default context size', kind: 'int', help: '0 uses the model\'s own.' },
      { key: 'gpu_layers', label: 'Default GPU layers', kind: 'int', min: 0, help: '999 puts every layer that fits on the GPU; 0 runs on the CPU.' },
      { key: 'parallel', label: 'Parallel requests per model', kind: 'int', min: 1 },
      { key: 'fail_fast_busy', label: 'Answer busy when every slot is taken', kind: 'bool', help: 'On: a request that finds no free slot gets busy (503, Retry-After 1) at once, instead of waiting in the queue. Off by default.' },
      { key: 'autoload', label: 'Load a model on its first request', kind: 'bool' },
      { key: 'load_timeout_s', label: 'Load timeout (seconds)', kind: 'int', min: 10 },
      { key: 'default_model', label: 'Model used when a request names none', kind: 'model', nullable: true },
      { key: 'extra_args', label: 'Extra llama-server arguments', kind: 'lines', help: 'One per line, given to every model.' },
      { key: 'llama_server', label: 'llama-server program', kind: 'path', nullable: true, help: 'Empty uses the one built in.' },
      { key: 'ollama_url', label: 'Ollama address', kind: 'text', help: 'Empty never uses Ollama.' },
    ],
  },
  {
    title: 'Forge (training)',
    help: 'Where MM trains and keeps its own models. Models under the data folder\'s gguf folders are listed as MM models.',
    fields: [
      { key: 'forge_dir', label: 'Forge environment folder', kind: 'path', nullable: true },
      { key: 'forge_data_dir', label: 'Forge data folder (weights, outputs)', kind: 'path', nullable: true },
      { key: 'forge_src', label: 'Forge source folder', kind: 'path', nullable: true },
      { key: 'forge_python', label: 'Python for the Forge', kind: 'path', nullable: true },
    ],
  },
  {
    title: 'Speech and images',
    fields: [
      { key: 'whisper_model', label: 'Speech to text model', kind: 'text', nullable: true, help: 'For example base.en.' },
      { key: 'tts_model', label: 'Text to speech model', kind: 'text', nullable: true },
      { key: 'tts_voice', label: 'Voice', kind: 'text', nullable: true },
      { key: 'image_model', label: 'Image model', kind: 'text', nullable: true },
    ],
  },
  {
    title: 'Hosting',
    help: 'Serving other machines also needs a peer key and the host option when the hub starts.',
    fields: [{ key: 'allow_network', label: 'Allow the hub to listen beyond this machine', kind: 'bool' }],
  },
]

const known = new Set(GROUPS.flatMap((g) => g.fields.map((f) => f.key)))
const original = ref<Record<string, any> | null>(null)
const form = ref<Record<string, any> | null>(null)
const modelIds = ref<string[]>([])
const error = ref('')
const note = ref('')
const saving = ref(false)

const extra = computed<Field[]>(() => {
  const o = original.value || {}
  return Object.keys(o)
    .filter((k) => !known.has(k) && (o[k] === null || typeof o[k] !== 'object'))
    .map((k) => ({
      key: k,
      label: k.replace(/_/g, ' '),
      kind: (typeof o[k] === 'boolean' ? 'bool' : typeof o[k] === 'number' ? 'int' : 'text') as Kind,
      nullable: o[k] === null,
    }))
})
const sections = computed(() => {
  const o = original.value || {}
  const s = GROUPS.map((g) => ({ ...g, fields: g.fields.filter((f) => f.key in o) })).filter((g) => g.fields.length)
  if (extra.value.length) s.push({ title: 'More', help: undefined, fields: extra.value })
  return s
})
const allFields = computed(() => sections.value.flatMap((s) => s.fields))

// A list or a path becomes text in the form and back when saving.
const toForm = (f: Field, v: any) => (f.kind === 'lines' ? (v || []).map(String).join('\n') : v == null ? '' : v)
const fromForm = (f: Field, v: any) => {
  if (f.kind === 'lines') return String(v).split('\n').map((x) => x.trim()).filter(Boolean)
  if (f.kind === 'int') return Number(v)
  if (f.kind === 'bool') return !!v
  if (f.nullable && String(v).trim() === '') return null
  return typeof v === 'string' ? v.trim() : v
}
const same = (a: any, b: any) => JSON.stringify(a) === JSON.stringify(b)
const changed = (key: string) => {
  const f = allFields.value.find((x) => x.key === key)
  return !!f && !!form.value && !same(fromForm(f, form.value[key]), original.value![key] ?? (f.nullable ? null : original.value![key]))
}
const dirty = computed(() => !!form.value && allFields.value.some((f) => changed(f.key)))

function fill(cfg: Record<string, any>) {
  original.value = cfg
  const f: Record<string, any> = {}
  for (const fd of allFieldsFor(cfg)) f[fd.key] = toForm(fd, cfg[fd.key])
  form.value = f
}
function allFieldsFor(cfg: Record<string, any>): Field[] {
  const o = original.value
  original.value = cfg
  const r = allFields.value
  original.value = o
  return r
}
const revert = () => original.value && fill(original.value)

async function load() {
  error.value = ''
  try {
    fill(await call('config.get', {}))
    try {
      modelIds.value = (await listModels()).filter(isChatModel).map((m) => m.id)
    } catch (_) {
      /* the picker is then empty */
    }
  } catch (e: any) {
    error.value = `Could not read the settings: ${e?.message || e}`
  }
}

async function save() {
  const patch: Record<string, any> = {}
  for (const f of allFields.value) if (changed(f.key)) patch[f.key] = fromForm(f, form.value![f.key])
  saving.value = true
  error.value = note.value = ''
  try {
    fill(await call('config.set', { settings: patch }))
    note.value = `Saved: ${Object.keys(patch).join(', ')}.`
  } catch (e: any) {
    error.value = `Not saved: ${e?.message || e}`
  } finally {
    saving.value = false
  }
}

onMounted(load)
</script>

<style scoped>
.gh { margin-bottom: 8px; }
.fields { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 12px; }
.wide { grid-column: 1 / -1; }
.chk { width: 18px; height: 18px; align-self: flex-start; }
</style>

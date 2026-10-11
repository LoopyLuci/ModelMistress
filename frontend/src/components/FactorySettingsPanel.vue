<template>
  <section class="card" aria-label="factory settings">
    <div class="row">
      <h3 class="grow">Factory settings</h3>
      <button class="btn" :disabled="busy" @click="load">Reload</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>
    <form v-if="cur" @submit.prevent="save">
      <div class="formgrid">
        <label class="field">Embedder
          <select v-model="cur.embedder"><option v-for="e in ['km', 'hub', 'hash']" :key="e" :value="e">{{ e }}</option></select>
        </label>
        <label class="field">Embedding model (when the embedder is hub)
          <input v-model="cur.embed_model" type="text" />
        </label>
        <label class="field">Promotion policy
          <select v-model="cur.promotion"><option v-for="e in ['gated', 'propose', 'auto', 'off']" :key="e" :value="e">{{ e }}</option></select>
        </label>
        <label v-for="k in NUMBERS" :key="k" class="field">{{ k.replace(/_/g, ' ') }}
          <input v-model.number="(cur as any)[k]" type="number" step="any" />
        </label>
      </div>
      <label class="row small"><input v-model="cur.auto_add_vocabulary" type="checkbox" /> Add tag values the author proposes to the vocabulary at once</label>
      <div class="row">
        <button class="btn primary" :disabled="busy || !dirty">Save settings</button>
        <span v-if="dirty" class="muted small">unsaved changes</span>
      </div>
    </form>

    <h4>Embedder</h4>
    <p class="muted small">
      Makes a hub model (an embedding GGUF) the factory's embedder under the name <span class="mono">{{ cur?.embed_model || 'mm-embed-1' }}</span>
      and points the KM runtime at it.
    </p>
    <form class="row" @submit.prevent="setup">
      <input v-model="model" type="text" list="embed-models" class="grow" placeholder="hub model id of an embedding GGUF" aria-label="embedding model" required />
      <datalist id="embed-models"><option v-for="m in models" :key="m" :value="m"></option></datalist>
      <button class="btn" :disabled="busy">Set up the embedder</button>
    </form>
    <p v-if="setupError" class="error-box" role="alert">{{ setupError }}</p>
    <dl v-if="result" class="kv" aria-label="embedder setup result">
      <dt>name</dt><dd class="mono">{{ result.name }}</dd>
      <dt>from</dt><dd class="mono">{{ result.from }}</dd>
      <dt>file</dt><dd class="mono">{{ result.path }} ({{ result.how }})</dd>
      <dt v-if="result.job">job</dt><dd v-if="result.job" class="mono">{{ result.job }}</dd>
      <dt v-if="result.note">note</dt><dd v-if="result.note">{{ result.note }}</dd>
    </dl>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { factory, modelIds, type FactorySettings } from '@/api'

const NUMBERS = [
  'min_uplift', 'review_threshold', 'tag_threshold', 'tag_floor', 'dedupe_threshold', 'leak_threshold', 'chunk_chars',
  'max_files', 'max_file_bytes', 'max_total_bytes', 'max_url_bytes', 'max_retries', 'gen_max_tokens', 'eval_notes',
]
const cur = ref<FactorySettings | null>(null)
const orig = ref('')
const models = ref<string[]>([])
const model = ref('')
const busy = ref(false)
const error = ref('')
const note = ref('')
const setupError = ref('')
const result = ref<any>(null)
const dirty = computed(() => !!cur.value && JSON.stringify(cur.value) !== orig.value)

async function load() {
  try {
    cur.value = await factory.settings()
    orig.value = JSON.stringify(cur.value)
    error.value = ''
  } catch (e: any) {
    error.value = e.message
  }
}
async function save() {
  if (!cur.value) return
  busy.value = true
  error.value = ''
  note.value = ''
  try {
    cur.value = await factory.setSettings({ ...cur.value })
    orig.value = JSON.stringify(cur.value)
    note.value = 'Settings saved.'
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
async function setup() {
  busy.value = true
  setupError.value = ''
  result.value = null
  try {
    result.value = await factory.embedderSetup(model.value.trim())
    await load()
  } catch (e: any) {
    setupError.value = e.message
  } finally {
    busy.value = false
  }
}
onMounted(() => {
  load()
  modelIds().then((m) => (models.value = m)).catch(() => {})
})
</script>

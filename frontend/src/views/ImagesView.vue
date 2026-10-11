<template>
  <div class="page">
    <h2>Images</h2>
    <p v-if="off" class="card muted" role="status">Not available in this build: {{ off }}</p>
    <form v-else class="card" @submit.prevent="go">
      <h3>Generate</h3>
      <textarea v-model="prompt" rows="3" placeholder="What to draw" aria-label="Prompt"></textarea>
      <div class="formgrid top">
        <label class="field">Size
          <select v-model="size"><option>512x512</option><option>768x768</option><option>1024x1024</option><option>768x512</option><option>512x768</option></select>
        </label>
        <label class="field">Pictures
          <input v-model.number="n" type="number" min="1" max="4" />
        </label>
        <label class="field">Steps (blank = quality default)
          <input v-model.number="steps" type="number" min="1" max="150" />
        </label>
        <label class="field">Seed (blank = random)
          <input v-model.number="seed" type="number" />
        </label>
      </div>
      <div class="row">
        <button class="btn primary" :disabled="!prompt.trim() || running">{{ running ? 'Generating...' : 'Generate' }}</button>
        <button v-if="running" class="btn" type="button" @click="cancel">Stop</button>
        <progress v-if="running" :value="pct" max="100" class="grow" aria-label="progress"></progress>
        <span v-if="running" class="muted small">{{ msg }}</span>
      </div>
      <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    </form>

    <section v-if="gallery.length" class="card">
      <h3>Gallery</h3>
      <div class="gal">
        <figure v-for="g in gallery" :key="g.name">
          <img v-if="urls[g.name]" :src="urls[g.name]" :alt="g.prompt" />
          <figcaption class="muted small">{{ g.prompt }}<br />seed {{ g.seed }}, {{ g.model }}</figcaption>
        </figure>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { call } from '@/transport'
import { hubFetch } from '@/chat/hub'

interface Item { name: string; prompt: string; seed: number; model: string }
const KEY = 'mm_images'
const prompt = ref('')
const size = ref('512x512')
const n = ref(1)
const steps = ref<number | null>(null)
const seed = ref<number | null>(null)
const running = ref(false)
const pct = ref(0)
const msg = ref('')
const error = ref('')
const off = ref('')
const gallery = ref<Item[]>(JSON.parse(localStorage.getItem(KEY) || '[]'))
const urls = reactive<Record<string, string>>({})
let job = ''
let stopped = false

async function show(it: Item) {
  try {
    const r = await hubFetch(`/v1/images/files/${it.name}`)
    if (r.ok) urls[it.name] = URL.createObjectURL(await r.blob())
  } catch (_) {
    /* the file is gone */
  }
}
async function go() {
  error.value = ''
  running.value = true
  stopped = false
  pct.value = 0
  msg.value = 'starting'
  try {
    const a: Record<string, unknown> = { prompt: prompt.value.trim(), n: n.value, size: size.value }
    if (steps.value) a.steps = steps.value
    if (seed.value != null && (seed.value as any) !== '') a.seed = seed.value
    let j = await call('image.generate', a)
    job = j.job
    while (j.status === 'running') {
      pct.value = j.percent || 0
      msg.value = j.message || ''
      await new Promise((ok) => setTimeout(ok, 700))
      j = await call('job.get', { job })
    }
    if (j.status !== 'done') {
      if (!stopped) error.value = j.message || `The job ${j.status}.`
      return
    }
    for (const f of j.result.files as string[]) {
      const it = { name: f.split(/[\\/]/).pop()!, prompt: prompt.value.trim(), seed: j.result.seed, model: j.result.model }
      gallery.value.unshift(it)
      show(it)
    }
    localStorage.setItem(KEY, JSON.stringify(gallery.value.slice(0, 40)))
  } catch (e: any) {
    if (e?.status === 501 || e?.status === 404) off.value = String(e.message)
    else error.value = String(e?.message || e)
  } finally {
    running.value = false
  }
}
async function cancel() {
  stopped = true
  try {
    await call('job.cancel', { job })
  } catch (_) {
    /* already over */
  }
}
onMounted(() => gallery.value.forEach(show))
onBeforeUnmount(() => Object.values(urls).forEach((u) => URL.revokeObjectURL(u)))
</script>

<style scoped>
.top { margin: 10px 0; }
.gal { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 12px; }
.gal img { width: 100%; border-radius: 8px; border: 1px solid var(--border); display: block; }
figure { margin: 0; }
</style>

<template>
  <div class="page">
    <h2>Vision</h2>
    <p v-if="off" class="card muted" role="status">Not available in this build: {{ off }}</p>
    <section v-else class="card">
      <div class="drop" :class="{ over }" @dragover.prevent="over = true" @dragleave="over = false" @drop.prevent="onDrop">
        <p>Drop a picture here, or <label class="btn">choose one<input type="file" accept="image/*" hidden @change="onPick" /></label></p>
      </div>
      <div class="row top">
        <label class="field">What to do
          <select v-model="what" @change="run()"><option value="detect">Find objects (boxes)</option><option value="classify">Classify (labels)</option></select>
        </label>
        <label class="field">Model (blank = the only one)
          <input v-model="model" type="text" @change="run()" />
        </label>
        <span v-if="busy" class="muted">Working...</span>
      </div>
      <p v-if="error" class="error-box" role="alert">{{ error }}</p>
      <div v-if="src" class="stage">
        <canvas ref="canvas"></canvas>
      </div>
      <ul v-if="result?.predictions" class="list">
        <li v-for="p in result.predictions" :key="p.class">{{ p.label }}: {{ (p.score * 100).toFixed(1) }}%</li>
      </ul>
      <ul v-if="result?.detections" class="list">
        <li v-for="(d, i) in result.detections" :key="i">{{ d.label }}: {{ (d.score * 100).toFixed(1) }}%</li>
        <li v-if="!result.detections.length" class="muted">Nothing found.</li>
      </ul>
    </section>
  </div>
</template>

<script setup lang="ts">
import { nextTick, ref } from 'vue'
import { failure, hubFetch } from '@/chat/hub'

const off = ref('')
const error = ref('')
const busy = ref(false)
const over = ref(false)
const what = ref('detect')
const model = ref('')
const src = ref('')
const result = ref<any>(null)
const canvas = ref<HTMLCanvasElement | null>(null)

function readFile(f: File) {
  const fr = new FileReader()
  fr.onload = () => {
    src.value = String(fr.result)
    run()
  }
  fr.readAsDataURL(f)
}
const onPick = (e: Event) => {
  const el = e.target as HTMLInputElement
  if (el.files?.[0]) readFile(el.files[0])
  el.value = ''
}
const onDrop = (e: DragEvent) => {
  over.value = false
  const f = e.dataTransfer?.files?.[0]
  if (f) readFile(f)
}

async function run() {
  if (!src.value) return
  busy.value = true
  error.value = ''
  result.value = null
  try {
    const r = await hubFetch(`/v1/vision/${what.value}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ image: src.value, ...(model.value.trim() ? { model: model.value.trim() } : {}) }),
    })
    if (!r.ok) throw await failure(r)
    result.value = await r.json()
  } catch (e: any) {
    if (e?.status === 501 || e?.status === 404) off.value = String(e.message)
    else error.value = String(e?.message || e)
  } finally {
    busy.value = false
  }
  await nextTick()
  draw()
}

function draw() {
  const c = canvas.value
  if (!c || !src.value) return
  const img = new Image()
  img.onload = () => {
    c.width = img.width
    c.height = img.height
    const g = c.getContext('2d')!
    g.drawImage(img, 0, 0)
    const lw = Math.max(2, Math.round(img.width / 300))
    g.lineWidth = lw
    g.font = `${Math.max(12, Math.round(img.width / 40))}px sans-serif`
    for (const d of result.value?.detections || []) {
      const b = d.box
      g.strokeStyle = '#ef4444'
      g.strokeRect(b.x1, b.y1, b.x2 - b.x1, b.y2 - b.y1)
      const t = `${d.label} ${(d.score * 100).toFixed(0)}%`
      const w = g.measureText(t).width + 8
      const h = parseInt(g.font) + 6
      g.fillStyle = '#ef4444'
      g.fillRect(b.x1, Math.max(0, b.y1 - h), w, h)
      g.fillStyle = '#fff'
      g.fillText(t, b.x1 + 4, Math.max(h - 5, b.y1 - 5))
    }
  }
  img.src = src.value
}
</script>

<style scoped>
.drop { border: 2px dashed var(--border); border-radius: 10px; padding: 18px; text-align: center; }
.drop.over { border-color: var(--accent); }
.top { margin: 10px 0; }
.stage canvas { max-width: 100%; height: auto; border-radius: 8px; border: 1px solid var(--border); }
.list { margin: 8px 0 0 1.2em; }
</style>

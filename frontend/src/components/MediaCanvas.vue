<template>
  <aside class="canvas" aria-label="Media canvas">
    <header class="head">
      <strong>Canvas</strong>
      <span class="muted small">{{ assets.length }} {{ assets.length === 1 ? 'item' : 'items' }}</span>
      <span class="grow"></span>
      <button v-if="current" class="mini" type="button" @click="download">Download</button>
      <button v-if="current" class="mini" type="button" @click="fullscreen">Full screen</button>
      <button class="mini" type="button" aria-label="Close the canvas" @click="emit('close')">Close</button>
    </header>

    <div v-if="assets.length > 1" class="strip" role="tablist">
      <button
        v-for="a in assets"
        :key="a.id"
        class="chip"
        :class="{ on: current && a.id === current.id }"
        role="tab"
        type="button"
        :aria-selected="!!current && a.id === current.id"
        :title="a.title"
        @click="emit('select', a.id)"
      >
        <span class="kind">{{ label(a.kind) }}</span> {{ short(a.title) }}
      </button>
    </div>

    <div ref="stage" class="stage">
      <p v-if="!current" class="muted pad">Pictures, video, audio, 3D models, drawings and pages that models make or mention in this chat show here.</p>
      <p v-else-if="error" class="error-box small" role="alert">{{ error }}</p>
      <p v-else-if="loading" class="muted pad">Loading...</p>
      <div v-else-if="current && remote" class="pad">
        <p class="muted small">This file is on another site, so the page does not load it by itself (a reply could use that to track you).</p>
        <p><a :href="current.src" target="_blank" rel="noopener noreferrer">Open {{ current.title }} in a new tab</a></p>
      </div>
      <template v-else-if="current && url">
        <div v-if="current.kind === 'image' || current.kind === 'svg'" class="imgwrap" @wheel.prevent="onWheel">
          <img :src="url" :alt="current.title" :style="zoomStyle" draggable="false" @pointerdown="dragStart" />
          <div class="zoom">
            <button class="mini" type="button" aria-label="Zoom out" @click="zoom = Math.max(0.2, zoom / 1.25)">-</button>
            <button class="mini" type="button" @click="resetZoom">{{ Math.round(zoom * 100) }}%</button>
            <button class="mini" type="button" aria-label="Zoom in" @click="zoom = Math.min(8, zoom * 1.25)">+</button>
          </div>
        </div>
        <video v-else-if="current.kind === 'video'" :src="url" controls loop playsinline class="media"></video>
        <div v-else-if="current.kind === 'audio'" class="audiobox">
          <p class="muted small">{{ current.title }}</p>
          <audio :src="url" controls class="audio"></audio>
        </div>
        <ModelViewer v-else-if="current.kind === 'model' && current.format" :url="url" :format="current.format" :title="current.title" />
        <iframe v-else-if="current.kind === 'pdf'" :src="url" class="frame" title="document"></iframe>
        <iframe v-else-if="current.kind === 'html'" :src="url" sandbox="" class="frame" title="page (scripts are off)"></iframe>
        <pre v-else-if="current.kind === 'text'" class="text">{{ text }}</pre>
        <p v-else class="muted pad">This kind of file cannot be shown here. Download it to open it.</p>
      </template>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, defineAsyncComponent, onBeforeUnmount, ref, watch } from 'vue'
import { hubFetch } from '@/chat/hub'
import { isHubPath, type Asset, type AssetKind } from '@/chat/media'

const ModelViewer = defineAsyncComponent(() => import('./ModelViewer.vue'))

const props = defineProps<{ assets: Asset[]; activeId: string }>()
const emit = defineEmits<{ (e: 'close'): void; (e: 'select', id: string): void }>()

const current = computed<Asset | null>(() => props.assets.find((a) => a.id === props.activeId) || props.assets[props.assets.length - 1] || null)
const url = ref('')
const text = ref('')
const loading = ref(false)
const error = ref('')
const remote = ref(false)
const stage = ref<HTMLElement | null>(null)
const zoom = ref(1)
const pan = ref({ x: 0, y: 0 })
let owned = ''

const zoomStyle = computed(() => ({ transform: `translate(${pan.value.x}px, ${pan.value.y}px) scale(${zoom.value})` }))
const label = (k: AssetKind) => ({ image: 'Image', video: 'Video', audio: 'Audio', model: '3D', svg: 'Drawing', html: 'Page', pdf: 'PDF', text: 'Text', file: 'File' })[k]
const short = (s: string) => (s.length > 26 ? s.slice(0, 24) + '...' : s)

function release() {
  if (owned) URL.revokeObjectURL(owned)
  owned = ''
}

/** The address the page can load: hub paths are fetched with the owner token into a blob, inline sources become a blob. */
async function load(a: Asset | null) {
  release()
  url.value = ''
  text.value = ''
  error.value = ''
  remote.value = false
  zoom.value = 1
  pan.value = { x: 0, y: 0 }
  if (!a) return
  // an http(s) file on another site is never fetched by the page: the policy blocks it and it would tell that site who looks
  if (a.inline == null && /^https?:/i.test(a.src)) {
    remote.value = true
    return
  }
  loading.value = true
  try {
    let src = a.src
    if (a.inline != null) {
      const type = a.kind === 'svg' ? 'image/svg+xml' : 'text/html'
      owned = URL.createObjectURL(new Blob([a.inline], { type }))
      src = owned
    } else if (isHubPath(a.src)) {
      const r = await hubFetch(a.src)
      if (!r.ok) throw new Error(`${r.status} ${r.statusText}`)
      owned = URL.createObjectURL(await r.blob())
      src = owned
    }
    if (a.kind === 'text') text.value = await (await fetch(src)).text()
    url.value = src
  } catch (e: any) {
    error.value = `Could not load ${a.title}: ${e?.message || e}`
  } finally {
    loading.value = false
  }
}

watch(current, (a, b) => { if (a?.id !== b?.id || a?.src !== b?.src) load(a) }, { immediate: true })
onBeforeUnmount(release)

function onWheel(e: WheelEvent) {
  zoom.value = Math.min(8, Math.max(0.2, zoom.value * (e.deltaY < 0 ? 1.1 : 1 / 1.1)))
}
function resetZoom() {
  zoom.value = 1
  pan.value = { x: 0, y: 0 }
}
function dragStart(e: PointerEvent) {
  const start = { x: e.clientX - pan.value.x, y: e.clientY - pan.value.y }
  const move = (m: PointerEvent) => (pan.value = { x: m.clientX - start.x, y: m.clientY - start.y })
  const up = () => {
    window.removeEventListener('pointermove', move)
    window.removeEventListener('pointerup', up)
  }
  window.addEventListener('pointermove', move)
  window.addEventListener('pointerup', up)
}
function download() {
  if (!url.value || !current.value) return
  const a = document.createElement('a')
  a.href = url.value
  a.download = current.value.title.replace(/[^\w.\- ]+/g, '_') || 'asset'
  document.body.appendChild(a)
  a.click()
  a.remove()
}
function fullscreen() {
  stage.value?.requestFullscreen?.().catch(() => {})
}
</script>

<style scoped>
.canvas { display: flex; flex-direction: column; min-width: 0; height: 100%; border-left: 1px solid var(--border); background: var(--panel); }
.head { display: flex; align-items: center; gap: 8px; padding: 6px 10px; border-bottom: 1px solid var(--border); }
.mini { background: none; border: 1px solid var(--border); border-radius: 6px; padding: 1px 8px; font-size: 0.75rem; color: var(--muted); }
.mini:hover { border-color: var(--accent); color: var(--text); }
.strip { display: flex; gap: 4px; overflow-x: auto; padding: 6px 8px; border-bottom: 1px solid var(--border); }
.chip { flex: none; background: var(--panel-2); border: 1px solid var(--border); border-radius: 14px; padding: 2px 10px; font-size: 0.78rem; color: var(--muted); white-space: nowrap; }
.chip.on { border-color: var(--accent); color: var(--text); }
.kind { opacity: 0.7; margin-right: 4px; }
.stage { position: relative; flex: 1; min-height: 240px; overflow: auto; display: flex; align-items: stretch; justify-content: center; background: var(--bg); }
.imgwrap { position: relative; flex: 1; overflow: hidden; display: flex; align-items: center; justify-content: center; cursor: grab; }
.imgwrap img { max-width: 100%; max-height: 100%; object-fit: contain; user-select: none; transform-origin: center; }
.zoom { position: absolute; right: 8px; bottom: 8px; display: flex; gap: 4px; }
.media { width: 100%; max-height: 100%; align-self: center; background: #000; }
.audiobox { align-self: center; width: 100%; padding: 16px; }
.audio { width: 100%; }
.frame { width: 100%; height: 100%; border: 0; background: #fff; }
.text { margin: 0; padding: 10px 12px; width: 100%; white-space: pre-wrap; word-break: break-word; font-family: var(--mono); font-size: 0.85rem; }
.pad { padding: 14px; }
</style>

<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Status</h2>
      <span class="pill" :class="up ? 'good' : 'bad'">{{ up ? 'hub running' : 'hub not reachable' }}</span>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>

    <div v-if="st" class="grid">
      <section class="card">
        <h3>Hub</h3>
        <dl class="kv">
          <dt>Version</dt><dd>{{ st.version }}</dd>
          <dt>Up for</dt><dd>{{ dur(st.uptime_s) }}</dd>
          <dt>Process</dt><dd>{{ st.pid }}<span v-if="st.system?.hub_rss_mb"> using {{ gb(st.system.hub_rss_mb) }}</span></dd>
          <dt>Engine</dt><dd>llama.cpp {{ st.llama_version || '' }}{{ st.llama_built_in ? ' (built in)' : '' }}</dd>
          <dt>Models known</dt><dd>{{ st.models_known }}</dd>
        </dl>
      </section>

      <section class="card">
        <h3>Memory</h3>
        <template v-if="st.system">
          <dl class="kv">
            <dt>System RAM</dt><dd>{{ gb(st.system.total_mb - st.system.available_mb) }} used of {{ gb(st.system.total_mb) }}</dd>
            <dt>Loaded models, RAM</dt><dd>{{ gb(ramTotal) }}</dd>
            <dt>Loaded models, GPU</dt><dd>{{ gpuTotal ? gb(gpuTotal) : 'none reported' }}</dd>
          </dl>
          <progress :value="st.system.total_mb - st.system.available_mb" :max="st.system.total_mb" class="prog" aria-label="RAM in use"></progress>
          <p class="muted small">GPU figures are the buffers llama.cpp reports for each loaded model; it does not report the card's total.</p>
        </template>
      </section>

      <section class="card wide">
        <h3>Loaded models</h3>
        <p v-if="!st.loaded.length" class="muted">Nothing loaded. <router-link to="/models">Load a model</router-link> or just start a <router-link to="/">chat</router-link>.</p>
        <div v-for="l in st.loaded" :key="l.model" class="row item">
          <strong>{{ l.model }}</strong>
          <span v-if="l.rss_mb != null" class="pill">{{ gb(l.rss_mb) }} RAM</span>
          <span v-for="b in l.buffers" :key="b.what" class="pill">{{ b.what }} {{ gb(b.mib) }}</span>
          <span class="muted small">up {{ dur(l.uptime_s) }}, idle {{ dur(l.idle_s) }}</span>
        </div>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { call } from '@/transport'

const st = ref<any>(null)
const up = ref(true)
const error = ref('')
let timer: number | undefined

const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`)
const dur = (s: number) => (s < 120 ? `${s} s` : s < 7200 ? `${Math.round(s / 60)} min` : s < 172800 ? `${(s / 3600).toFixed(1)} h` : `${(s / 86400).toFixed(1)} days`)
const ramTotal = computed(() => (st.value?.loaded || []).reduce((n: number, l: any) => n + (l.rss_mb || 0), 0))
const gpuTotal = computed(() =>
  (st.value?.loaded || []).reduce(
    (n: number, l: any) => n + (l.buffers || []).filter((b: any) => !/^(CPU|Vulkan_Host|CUDA_Host)/i.test(b.what)).reduce((m: number, b: any) => m + b.mib, 0),
    0,
  ),
)

async function tick() {
  try {
    st.value = await call('service.status', {})
    up.value = true
    error.value = ''
  } catch (e: any) {
    up.value = false
    error.value = String(e?.message || e)
  }
}
onMounted(() => {
  tick()
  timer = window.setInterval(tick, 3000)
})
onBeforeUnmount(() => window.clearInterval(timer))
</script>

<style scoped>
.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 0 14px; }
.wide { grid-column: 1 / -1; }
.prog { width: 100%; margin-top: 8px; }
.item { padding: 4px 0; }
</style>

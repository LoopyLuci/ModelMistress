<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Residency</h2>
      <span class="pill" :class="live ? 'ok' : 'error'" role="status">{{ live ? 'live' : 'hub not reachable' }}</span>
      <label class="row small"><input v-model="paused" type="checkbox" /> pause updates</label>
      <button class="btn danger" :disabled="!r || !r.resident.length" @click="confirmAll = true">Unload all</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>

    <div v-if="r" class="split">
      <div>
        <div class="card">
          <h3>Tiers</h3>
          <TierBar label="Disk" :used="r.tiers.disk.used" :budget="r.tiers.disk.budget" :items="[]" />
          <TierBar label="RAM" :used="r.tiers.ram.used" :budget="r.tiers.ram.budget" :items="inTier('ram')" />
          <TierBar label="VRAM" :used="r.tiers.vram.used" :budget="r.tiers.vram.budget" :items="inTier('vram')" />
          <p class="muted small">
            Segments are resident KMs sized by bytes (click one to open it). Striped = pinned, outlined = in use by a request and
            protected from eviction. Budgets are set with <span class="mono">[km]</span> in the hub config.
          </p>
          <table v-if="coreNames.length" class="t" aria-label="per-core budgets">
            <thead><tr><th>core</th><th class="num">RAM</th><th class="num">VRAM</th></tr></thead>
            <tbody>
              <tr v-for="c in coreNames" :key="c">
                <td>{{ c }}</td>
                <td class="num">{{ fmtBytes(r.cores[c].ram.used) }} / {{ fmtBytes(r.cores[c].ram.budget) }}</td>
                <td class="num">{{ fmtBytes(r.cores[c].vram.used) }} / {{ fmtBytes(r.cores[c].vram.budget) }}</td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="card">
          <h3>Resident ({{ r.resident.length }})</h3>
          <div class="tablewrap">
            <table class="t" aria-label="resident KMs">
              <thead><tr><th>KM</th><th>tier</th><th class="num">size</th><th class="num">heat</th><th class="num">idle</th><th>state</th><th></th></tr></thead>
              <tbody>
                <tr v-for="x in bySize" :key="x.key">
                  <td>
                    <router-link :to="`/knowledge/${encodeURIComponent(x.id)}`" class="mono">{{ x.key }}</router-link>
                    <div class="muted small">{{ x.kind }}<span v-if="x.core"> on {{ x.core }}</span> &middot; attach {{ x.attach }}</div>
                  </td>
                  <td>{{ x.tier }}</td>
                  <td class="num">{{ fmtBytes(x.bytes) }}</td>
                  <td class="num" :title="`half-life ${r.half_life_s} s`">{{ x.heat }}</td>
                  <td class="num">{{ x.idle_s }} s</td>
                  <td>
                    <span v-if="x.pinned" class="pill pinned">pinned</span>
                    <span v-if="x.in_flight" class="pill pending">{{ x.in_flight }} in flight</span>
                  </td>
                  <td>
                    <button class="btn" :disabled="x.in_flight > 0" :title="x.in_flight ? 'in use by a request' : ''" @click="unload(x)">Unload</button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="!r.resident.length" class="muted">Nothing is resident. Load a KM from the Knowledge page.</p>
          <p v-if="r.note" class="muted small">{{ r.note }}</p>
        </div>
        <div v-if="r.pinned.length" class="card">
          <h3>Pinned ids</h3>
          <span v-for="p in r.pinned" :key="p" class="pill pinned">{{ p }}</span>
        </div>
      </div>

      <div class="card feed">
        <h3>Events</h3>
        <div class="row">
          <label class="field">kind
            <select v-model="kind">
              <option value="">all</option>
              <option v-for="k in KINDS" :key="k" :value="k">{{ k }}</option>
            </select>
          </label>
          <span class="muted small">{{ shownEvents.length }} shown</span>
        </div>
        <ul class="events" aria-live="polite" aria-label="residency events">
          <li v-for="e in shownEvents" :key="e.seq">
            <span class="muted mono">{{ time(e.at) }}</span>
            <span class="pill" :class="e.kind">{{ e.kind }}</span>
            <router-link :to="`/knowledge/${encodeURIComponent(idOf(e.key))}`" class="mono">{{ e.key }}</router-link>
            <span v-if="e.tier" class="muted small">{{ e.tier }}</span>
            <span v-if="e.bytes" class="muted small">{{ fmtBytes(e.bytes) }}</span>
            <div v-if="e.reason" class="reason">{{ e.reason }}</div>
          </li>
        </ul>
        <p v-if="!shownEvents.length" class="muted">No events yet.</p>
      </div>
    </div>

    <ConfirmDialog :open="confirmAll" title="Unload every KM?" action="Unload all" @confirm="unloadAll" @cancel="confirmAll = false">
      Everything resident is released, except what a request is using right now.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import TierBar from '@/components/TierBar.vue'
import { km, type KmEvent, type Resident, type Residency } from '@/api'
import { fmtBytes } from '@/util'

// The hub has no push channel for residency events yet, so the page polls km.events with the last sequence number.
const KINDS = ['load', 'unload', 'evict', 'refuse', 'verify', 'misdeclared', 'attach', 'detach', 'prefetch', 'error']
const r = ref<Residency | null>(null)
const events = ref<KmEvent[]>([])
const error = ref('')
const live = ref(true)
const paused = ref(false)
const kind = ref('')
const confirmAll = ref(false)
let timer = 0
let last = 0

const inTier = (t: 'ram' | 'vram'): Resident[] => (r.value?.resident || []).filter((x) => x.tier === t)
const bySize = computed(() => [...(r.value?.resident || [])].sort((a, b) => b.bytes - a.bytes))
const coreNames = computed(() => Object.keys(r.value?.cores || {}))
const shownEvents = computed(() => events.value.filter((e) => !kind.value || e.kind === kind.value))
const time = (ms: number) => new Date(ms).toLocaleTimeString()
const idOf = (key: string) => key.split('@')[0]

async function tick() {
  if (!paused.value) {
    try {
      r.value = await km.residency()
      const ev = await km.events({ since: last, limit: 100 })
      if (ev.events.length) {
        last = ev.last_seq ?? last
        events.value = [...ev.events.reverse(), ...events.value].slice(0, 300)
      }
      live.value = true
      error.value = ''
    } catch (e: any) {
      live.value = false
      error.value = e.message
    }
  }
  timer = window.setTimeout(tick, 2000)
}

async function unload(x: Resident) {
  try {
    await km.unload(x.id)
    await tick0()
  } catch (e: any) {
    error.value = e.message
  }
}
async function unloadAll() {
  confirmAll.value = false
  try {
    await km.unloadAll()
    await tick0()
  } catch (e: any) {
    error.value = e.message
  }
}
async function tick0() {
  window.clearTimeout(timer)
  await tick()
}

onMounted(tick)
onBeforeUnmount(() => window.clearTimeout(timer))
</script>

<style scoped>
.feed { max-height: 80vh; display: flex; flex-direction: column; }
.events { list-style: none; overflow-y: auto; margin-top: 8px; }
.events li { padding: 5px 0; border-bottom: 1px solid var(--border); font-size: 0.85rem; }
.events li > * { margin-right: 6px; }
.reason { color: var(--muted); margin-top: 1px; }
</style>

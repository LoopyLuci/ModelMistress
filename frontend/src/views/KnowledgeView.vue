<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Knowledge</h2>
      <button class="btn" @click="refresh">Refresh</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>

    <form class="card" role="search" @submit.prevent="run">
      <div class="row">
        <label class="grow row">
          <span class="sr-only">Search</span>
          <input
            v-model="f.q" type="text" class="grow" autocomplete="off"
            placeholder="search, e.g. cancel a tokio task  tag:lang:rust  domain:code/*  kind:adapter"
          />
        </label>
        <button class="btn primary">Search</button>
        <button type="button" class="btn" @click="clear">Clear</button>
      </div>
      <div class="row filters">
        <label class="field">kind
          <select v-model="f.kind" @change="run">
            <option value="">any</option>
            <option v-for="k in KINDS" :key="k" :value="k">{{ k }}</option>
          </select>
        </label>
        <label class="field">base (works with)
          <select v-model="f.base" @change="run">
            <option value="">any</option>
            <option v-for="b in bases" :key="b" :value="b">{{ b }}</option>
          </select>
        </label>
        <label class="field">channel
          <select v-model="f.channel" @change="run">
            <option value="">default</option>
            <option v-for="c in ['stable', 'candidate']" :key="c" :value="c">{{ c }}</option>
          </select>
        </label>
        <label class="field">status
          <select v-model="f.status">
            <option value="">any</option>
            <option v-for="s in ['resident', 'pinned', 'banned', 'absorbed']" :key="s" :value="s">{{ s }}</option>
          </select>
        </label>
      </div>
      <p class="muted small">
        Structured terms in the box are exact conditions: <span class="mono">tag:lang:rust</span>,
        <span class="mono">domain:code/*</span>, <span class="mono">kind:memory</span>. Absorbed KMs show up when the status
        filter asks for them.
      </p>
    </form>

    <template v-if="search">
      <div class="row bar">
        <h3 class="grow">{{ shown.length }} match(es) <span class="muted small">embedder {{ embedderText }}</span></h3>
        <label class="row small"><input v-model="expandAll" type="checkbox" /> show every score term</label>
      </div>
      <p v-for="n in search.notes" :key="n" class="muted small">{{ n }}</p>
      <div v-for="h in shown" :key="h.key" class="card">
        <div class="row">
          <span class="muted">#{{ h.rank }}</span>
          <router-link :to="detail(h)"><strong>{{ h.name || h.id }}</strong></router-link>
          <span class="mono muted">{{ h.key }}</span>
          <span class="pill">{{ h.kind }}</span>
          <span v-for="s in statusOf(h.key)" :key="s" class="pill" :class="s">{{ s }}</span>
          <span class="grow"></span>
          <strong class="mono" title="total score">{{ h.score.toFixed(4) }}</strong>
        </div>
        <div class="muted small">{{ h.domain }}</div>
        <details :open="expandAll || h.rank === 1" class="why">
          <summary>Why this score</summary>
          <ScoreBreakdown :hit="h" />
        </details>
      </div>
      <p v-if="!shown.length" class="muted">Nothing matches.</p>
      <details v-if="search.excluded.length" class="card">
        <summary>{{ search.excluded.length }} excluded, with reasons</summary>
        <ul class="excl">
          <li v-for="e in search.excluded" :key="e.key"><span class="mono">{{ e.key }}</span>: {{ e.reasons.join('; ') }}</li>
        </ul>
      </details>
    </template>

    <div v-else class="card tablewrap">
      <table class="t" aria-label="installed Knowledge Modules">
        <thead>
          <tr><th>name</th><th>version</th><th>kind</th><th>domain</th><th>base</th><th>channels</th><th>state</th><th class="num">size</th></tr>
        </thead>
        <tbody>
          <tr v-for="k in filteredList" :key="k.key">
            <td>
              <router-link :to="detail(k)"><strong>{{ k.name || k.id }}</strong></router-link>
              <div class="mono muted small">{{ k.id }}</div>
              <div class="tags"><span v-for="t in k.tags.slice(0, 5)" :key="t" class="pill">{{ t }}</span></div>
            </td>
            <td class="mono">{{ k.version }}</td>
            <td>{{ k.kind }}</td>
            <td class="muted">{{ k.domain }}</td>
            <td class="small">{{ k.base || '-' }}<span v-if="k.base_version" class="muted"> {{ k.base_version }}</span><span v-if="k.arch" class="muted"> {{ k.arch }}</span></td>
            <td><span v-for="c in k.channels" :key="c" class="pill">{{ c }}</span></td>
            <td><span v-for="s in statusOf(k.key)" :key="s" class="pill" :class="s">{{ s }}</span></td>
            <td class="num">{{ fmtBytes(k.bytes) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="!filteredList.length && !loading" class="muted">
        No Knowledge Modules{{ list.length ? ' match' : ' installed. Import one with: model-mistress km import (a folder or .mmkm file)' }}.
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import ScoreBreakdown from '@/components/ScoreBreakdown.vue'
import { km, type KmItem, type Search, type SearchHit } from '@/api'
import { fmtBytes } from '@/util'

const KINDS = ['adapter', 'memory', 'data', 'skill', 'tool', 'expert', 'projector', 'router', 'specialist', 'bundle']
const route = useRoute()
const router = useRouter()
const f = reactive({
  q: (route.query.q as string) || '', kind: (route.query.kind as string) || '', base: (route.query.base as string) || '',
  channel: (route.query.channel as string) || '', status: (route.query.status as string) || '',
})
const list = ref<KmItem[]>([])
const search = ref<Search | null>(null)
const loading = ref(false)
const error = ref('')
const expandAll = ref(false)
const byKey = computed(() => new Map(list.value.map((k) => [k.key, k])))

const absorbed = (k: KmItem) => Object.values(k.consolidation || {}).some((c) => c?.state === 'absorbed')
function statusOf(key: string): string[] {
  const k = byKey.value.get(key)
  if (!k) return []
  return [k.resident && 'resident', k.pinned && 'pinned', k.banned && 'banned', absorbed(k) && 'absorbed', k.misdeclared && 'misdeclared'].filter(Boolean) as string[]
}
const keep = (key: string) => !f.status || statusOf(key).includes(f.status)
// km.list carries base, base_version and arch, so the base filter needs no search; 'any' KMs fit every base.
const bases = computed(() => [...new Set([...list.value.map((k) => k.base).filter(Boolean), f.base].filter(Boolean))].sort() as string[])
const fitsBase = (k: KmItem) => !f.base || k.base === f.base || k.base === 'any'
const filteredList = computed(() => list.value.filter((k) => keep(k.key) && (!f.kind || k.kind === f.kind) && fitsBase(k)))
const shown = computed<SearchHit[]>(() => (search.value?.results || []).filter((h) => keep(h.key)))
const embedderText = computed(() => {
  const e = search.value?.embedder
  return typeof e === 'string' ? e : e?.id || e?.name || e?.kind || ''
})
const detail = (k: { id: string; version: string }) => ({ path: `/knowledge/${encodeURIComponent(k.id)}`, query: { version: k.version } })

async function run() {
  error.value = ''
  loading.value = true
  router.replace({ query: Object.fromEntries(Object.entries(f).filter(([, v]) => v)) })
  try {
    if (f.q.trim()) {
      search.value = await km.search({
        query: f.q, top_k: 50, include_absorbed: true,
        ...(f.kind ? { kinds: [f.kind] } : {}), ...(f.base ? { base: f.base } : {}), ...(f.channel ? { channel: f.channel } : {}),
      })
    } else {
      search.value = null
    }
  } catch (e: any) {
    error.value = e.message
  } finally {
    loading.value = false
  }
}
async function refresh() {
  try {
    list.value = await km.list()
  } catch (e: any) {
    error.value = e.message
  }
  await run()
}
function clear() {
  Object.keys(f).forEach((k) => ((f as any)[k] = ''))
  run()
}
onMounted(refresh)
</script>

<style scoped>
.filters { margin-top: 10px; align-items: flex-end; }
.tags { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 2px; }
.why { margin-top: 8px; }
.why summary { cursor: pointer; color: var(--muted); font-size: 0.85rem; }
.excl { padding-left: 18px; font-size: 0.85rem; }
</style>

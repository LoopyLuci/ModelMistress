<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/knowledge" class="btn">Back</router-link>
      <h2 class="grow">{{ d ? d.record.name || d.record.id : id }}</h2>
      <button class="btn" @click="load">Refresh</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>
    <p v-if="!d && !error" class="muted">Loading...</p>

    <template v-if="d">
      <div class="card">
        <div class="row">
          <span class="mono">{{ d.key }}</span>
          <span class="pill">{{ rec.kind }}</span>
          <span v-for="s in states" :key="s" class="pill" :class="s">{{ s }}</span>
          <span v-for="(v, c) in d.channels" :key="c" class="pill" :title="`channel ${c}`">{{ c }} {{ v.version }}</span>
        </div>
        <p class="desc">{{ rec.description }}</p>
        <div class="row actions" role="group" aria-label="actions">
          <button class="btn" :disabled="busy" @click="act('load')">Load</button>
          <button class="btn" :disabled="busy || !d.resident?.key" @click="act('unload')">Unload</button>
          <button class="btn" :disabled="busy" @click="act('pin')">{{ d.owner.pinned ? 'Unpin' : 'Pin' }}</button>
          <button class="btn" :disabled="busy" @click="d.owner.banned ? act('ban') : ask('ban')">{{ d.owner.banned ? 'Lift ban' : 'Ban' }}</button>
          <button class="btn" :disabled="busy" @click="act('export')">Export .mmkm</button>
          <router-link v-if="rec.kind === 'data'" :to="`/data/${encodeURIComponent(rec.id)}`" class="btn">Open in data studio</router-link>
          <span class="grow"></span>
          <button class="btn danger" :disabled="busy" @click="ask('remove')">Remove this version</button>
        </div>
        <p v-if="rec.misdeclared" class="error-box">Marked misdeclared: {{ rec.misdeclared }}. Loading is refused unless forced.</p>
        <div v-if="d.resident?.key" class="muted small">
          Resident in {{ d.resident.tier }}, {{ fmtBytes(d.resident.bytes) }}, heat {{ d.resident.heat }}, {{ d.resident.in_flight }} in flight, attach {{ d.resident.attach }}.
        </div>
      </div>

      <div class="split">
        <div>
          <div class="card">
            <h3>Manifest</h3>
            <dl class="kv">
              <dt>id</dt><dd class="mono">{{ rec.id }}</dd>
              <dt>version</dt><dd class="mono">{{ rec.version }}</dd>
              <dt>domain</dt><dd>{{ rec.domain }}</dd>
              <dt>license</dt><dd>{{ rec.license || '-' }}</dd>
              <dt>created</dt><dd>{{ created }}</dd>
              <dt>parent</dt><dd class="mono">{{ rec.parent || '-' }}</dd>
              <dt>base</dt><dd>{{ rec.base }}<span v-if="rec.base_version">, {{ rec.base_version }}</span><span v-if="rec.arch"> ({{ rec.arch }})</span></dd>
              <dt>modalities</dt><dd>{{ (rec.input || []).join(', ') }} to {{ (rec.output || []).join(', ') }}</dd>
              <dt>requires</dt><dd class="mono">{{ (rec.requires || []).join(', ') || '-' }}</dd>
              <dt>conflicts</dt><dd class="mono">{{ (rec.conflicts || []).join(', ') || '-' }}</dd>
              <dt>resources</dt><dd>RAM {{ fmtBytes(rec.ram_bytes) }}, VRAM {{ fmtBytes(rec.vram_bytes) }}, on disk {{ fmtBytes(rec.bytes) }}</dd>
              <dt>embedder</dt><dd>{{ rec.embed_model || '-' }}</dd>
              <dt>prior</dt><dd>{{ rec.prior }}</dd>
              <dt>descriptor</dt><dd>{{ rec.descriptor || '-' }}</dd>
              <dt>keywords</dt><dd>{{ (rec.keywords || []).join(', ') || '-' }}</dd>
              <dt>content id</dt><dd class="mono wrap">{{ rec.content_id }}</dd>
              <dt>requirements</dt>
              <dd>
                <span v-if="d.requires_check.ok" class="pill ok">resolved: {{ d.requires_check.set.length }} KM(s)</span>
                <span v-else class="pill error">{{ d.requires_check.problem }}</span>
              </dd>
            </dl>
            <p v-for="w in d.warnings" :key="w" class="muted small">warning: {{ w }}</p>
            <p v-if="d.unknown_keys?.length" class="muted small">Unknown keys kept: {{ d.unknown_keys.join(', ') }}</p>
          </div>

          <div class="card">
            <h3>Tags</h3>
            <table class="t" aria-label="tags">
              <thead><tr><th>tag</th><th>origin</th><th>confidence</th><th>vocabulary</th></tr></thead>
              <tbody>
                <tr v-for="t in d.tags" :key="t.tag">
                  <td class="mono">{{ t.tag }}</td>
                  <td>{{ t.origin }}</td>
                  <td>
                    <span class="meter" role="img" :aria-label="`confidence ${t.confidence}`"><span :style="{ width: t.confidence * 100 + '%' }"></span></span>
                    <span class="mono"> {{ Number(t.confidence).toFixed(2) }}</span>
                  </td>
                  <td><span v-if="t.approved" class="pill approved">approved</span><span v-else class="pill pending" :title="t.problem">{{ t.problem || 'not approved' }}</span></td>
                </tr>
              </tbody>
            </table>
            <p v-if="!d.tags.length" class="muted">No tags.</p>
          </div>

          <div class="card">
            <h3>Payload files</h3>
            <div class="tablewrap">
              <table class="t" aria-label="payload files">
                <thead><tr><th>path</th><th>role</th><th>format</th><th class="num">size</th><th>sha256</th></tr></thead>
                <tbody>
                  <tr v-for="p in d.payload" :key="p.path">
                    <td class="mono">{{ p.path }}</td><td>{{ p.role }}</td><td>{{ p.format }}</td>
                    <td class="num">{{ fmtBytes(p.bytes) }}</td>
                    <td class="mono" :title="p.sha256">{{ (p.sha256 || '').slice(0, 16) }}...</td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        </div>

        <div>
          <div class="card">
            <h3>Versions and channels</h3>
            <table class="t" aria-label="versions">
              <thead><tr><th>version</th><th>channels</th></tr></thead>
              <tbody>
                <tr v-for="v in d.versions" :key="v" :class="{ sel: v === rec.version }">
                  <td class="mono"><router-link :to="{ path: `/knowledge/${encodeURIComponent(rec.id)}`, query: { version: v } }">{{ v }}</router-link></td>
                  <td><span v-for="c in channelsAt(v)" :key="c" class="pill">{{ c }}</span></td>
                </tr>
              </tbody>
            </table>
            <form class="row mv" @submit.prevent="askMove">
              <label class="field">channel
                <input v-model="mv.channel" type="text" list="chans" style="width: 120px" />
                <datalist id="chans"><option v-for="(v, c) in d.channels" :key="c" :value="c"></option><option value="stable"></option><option value="candidate"></option></datalist>
              </label>
              <label class="field">to version
                <select v-model="mv.version"><option v-for="v in d.versions" :key="v" :value="v">{{ v }}</option></select>
              </label>
              <button class="btn" :disabled="!mv.channel || !mv.version || busy">Move channel</button>
            </form>
            <div v-for="(v, c) in d.channels" :key="c" class="row small">
              <span class="pill">{{ c }}</span> at <span class="mono">{{ v.version }}</span>
              <span class="muted">since {{ fmtWhen(v.moved_at) }}</span>
              <button v-if="rollback(String(c))" class="btn" :disabled="busy" @click="askRollback(String(c))">
                Roll back to {{ rollback(String(c)) }}
              </button>
            </div>
          </div>

          <div class="card">
            <h3>Quality and consolidation</h3>
            <dl class="kv">
              <dt>eval set</dt><dd class="mono">{{ m.quality?.eval_set || '-' }}</dd>
              <template v-for="(v, k) in m.quality?.scores || {}" :key="k"><dt>{{ k }}</dt><dd>{{ v }}</dd></template>
            </dl>
            <table v-if="consol.length" class="t" aria-label="consolidation per core">
              <thead><tr><th>core</th><th>state</th><th class="num">uplift</th><th class="num">steps</th><th class="num">last step</th></tr></thead>
              <tbody>
                <tr v-for="c in consol" :key="c.core">
                  <td>{{ c.core }}</td><td><span class="pill" :class="c.state">{{ c.state }}</span></td>
                  <td class="num">{{ c.uplift ?? '-' }}</td><td class="num">{{ c.steps ?? '-' }}</td><td class="num">{{ c.last_step ?? '-' }}</td>
                </tr>
              </tbody>
            </table>
            <p v-else class="muted">No consolidation recorded: the weights have not absorbed any of this.</p>
          </div>

          <div class="card">
            <h3>Provenance and signature</h3>
            <dl class="kv">
              <dt>created by</dt><dd>{{ m.provenance?.created_by || '-' }}</dd>
              <dt>training run</dt><dd class="mono">{{ m.provenance?.training_run || '-' }}</dd>
              <dt>teachers</dt><dd class="mono">{{ (m.provenance?.teachers || []).join(', ') || '-' }}</dd>
              <dt>trained on</dt>
              <dd>
                <template v-for="x in m.provenance?.data || []" :key="x">
                  <router-link :to="`/data/${encodeURIComponent(String(x).split('@')[0])}`" class="mono">{{ x }}</router-link>{{ ' ' }}
                </template>
                <span v-if="!(m.provenance?.data || []).length">-</span>
              </dd>
              <dt>installed from</dt><dd>{{ d.signature.source }}</dd>
              <dt>signature</dt>
              <dd>
                <span class="pill" :class="d.signature.state === 'verified' ? 'verified' : 'warn'">{{ d.signature.state }}</span>
                <span v-if="d.signature.unsigned_override" class="pill warn">owner override</span>
                <div v-if="d.signature.key" class="mono small wrap">{{ d.signature.key }}</div>
              </dd>
            </dl>
          </div>

          <div v-if="d.data" class="card">
            <h3>Data</h3>
            <p>
              {{ d.data.counts.total }} records: {{ d.data.counts.approved }} approved, {{ d.data.counts.pending }} pending,
              {{ d.data.counts.rejected }} rejected.
              <span v-if="d.data.working_copy?.[1]" class="pill pending">working copy has uncommitted edits</span>
            </p>
          </div>
          <div v-if="d.tool" class="card">
            <h3>Tool</h3>
            <p>{{ d.tool.provider }} / {{ d.tool.name }} ({{ d.tool.side_effects }}). {{ d.tool.note }}</p>
          </div>
          <div v-if="d.skill" class="card">
            <h3>Skill</h3>
            <p class="mono">{{ d.skill.instructions }}</p>
          </div>

          <details class="card">
            <summary>History ({{ d.history.length }})</summary>
            <ul class="hist">
              <li v-for="h in d.history" :key="h.seq">
                <span class="muted">{{ fmtWhen(h.at) }}</span> <strong>{{ h.action }}</strong>
                <span class="mono"> {{ h.key }}</span> <span class="muted small">{{ brief(h.detail) }}</span>
              </li>
            </ul>
          </details>
        </div>
      </div>
    </template>

    <ConfirmDialog :open="!!pending" :title="pending?.title || ''" :action="pending?.action || ''" @confirm="confirmed" @cancel="pending = null">
      {{ pending?.text }}
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { km } from '@/api'
import { fmtBytes, fmtWhen } from '@/util'

const props = defineProps<{ id: string }>()
const route = useRoute()
const router = useRouter()
const d = ref<any>(null)
const error = ref('')
const note = ref('')
const busy = ref(false)
const pending = ref<{ title: string; text: string; action: string; done: string; run: () => Promise<any> } | null>(null)
let removed = false
const mv = reactive({ channel: 'stable', version: '' })

const rec = computed(() => d.value.record)
const m = computed(() => d.value.manifest || {})
// a TOML datetime reaches the page as {"$__toml_private_datetime": "..."}
const created = computed(() => {
  const c = m.value.created
  return (c && typeof c === 'object' ? Object.values(c)[0] : c) || '-'
})
const states = computed(() => {
  const o = d.value
  return [o.resident?.key && 'resident', o.owner.pinned && 'pinned', o.owner.banned && 'banned',
    consol.value.some((c) => c.state === 'absorbed') && 'absorbed'].filter(Boolean) as string[]
})
const consol = computed(() =>
  Object.entries((d.value?.consolidation || {}) as Record<string, any>).map(([core, c]) => ({ core, ...c })),
)
const channelsAt = (v: string) => Object.entries(d.value.channels as Record<string, any>).filter(([, x]) => x.version === v).map(([c]) => c)

/** The version a channel pointed at before its latest move (from the move history), when it is still installed. */
function rollback(ch: string): string {
  const cur = d.value.channels[ch]?.version
  const last = (d.value.history as any[]).find((h) => h.action === 'channel' && h.key === `${rec.value.id}:${ch}`)
  const from = last?.detail?.from
  return last && last.detail?.to === cur && from && d.value.versions.includes(from) ? from : ''
}

function brief(x: any): string {
  return x == null ? '' : typeof x === 'object' ? Object.entries(x).map(([k, v]) => `${k}=${typeof v === 'object' ? JSON.stringify(v) : v}`).join(' ') : String(x)
}

async function load() {
  error.value = ''
  try {
    d.value = await km.show(props.id, (route.query.version as string) || undefined)
    mv.version = rec.value.version
    const first = Object.keys(d.value.channels)[0]
    if (first) mv.channel = first
  } catch (e: any) {
    error.value = e.message
  }
}
watch(() => [props.id, route.query.version], load, { immediate: true })

async function run(label: string, fn: () => Promise<any>) {
  busy.value = true
  error.value = ''
  note.value = ''
  try {
    const r = await fn()
    note.value = typeof r === 'string' ? r : label
    if (!removed) await load()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}

function act(a: 'load' | 'unload' | 'pin' | 'ban' | 'export') {
  const id = rec.value.id
  const v = rec.value.version
  if (a === 'load') return run('Loaded.', () => km.load(id, v))
  if (a === 'unload') return run('Unloaded.', () => km.unload(id, v))
  if (a === 'pin') return run(d.value.owner.pinned ? 'Unpinned.' : 'Pinned.', () => km.pin(id, !d.value.owner.pinned))
  if (a === 'ban') return run(d.value.owner.banned ? 'Ban lifted.' : 'Banned.', () => km.ban(id, !d.value.owner.banned))
  return run('Exporting...', async () => {
    const r = await km.export(id, v)
    return `Wrote ${r.path || 'the archive'} on the hub's machine.`
  })
}

function ask(a: 'ban' | 'remove') {
  const id = rec.value.id
  const v = rec.value.version
  pending.value =
    a === 'ban'
      ? { title: `Ban ${id}?`, action: 'Ban', done: 'Banned.', text: 'A banned KM is never selected or loaded, and is unloaded now. You can lift the ban later.', run: () => km.ban(id, true) }
      : { title: `Remove ${id}@${v}?`, action: 'Remove', done: 'Removed.', text: 'This deletes this version from the store. A resident KM must be unloaded first. It cannot be undone, but an exported .mmkm can be imported again.',
          run: async () => {
            await km.remove(id, v)
            removed = true
            router.push('/knowledge')
          } }
}

function askMove() {
  const id = rec.value.id
  const { channel, version } = mv
  const cur = d.value.channels[channel]?.version
  const fn = () => km.channelSet(id, channel, version, cur ?? '')
  const done = `${channel} now points at ${version}.`
  if (channel === 'stable') {
    pending.value = { title: `Move ${channel} to ${version}?`, action: 'Move', done, text: `Requests that follow the stable channel get ${version} from now on${cur ? ` (it points at ${cur})` : ''}. Moving it back is a rollback.`, run: fn }
  } else {
    run(done, fn)
  }
}
function askRollback(ch: string) {
  const to = rollback(ch)
  const id = rec.value.id
  const cur = d.value.channels[ch].version
  pending.value = { title: `Roll ${ch} back to ${to}?`, action: 'Roll back', done: `${ch} rolled back to ${to}.`, text: `${ch} moves from ${cur} to ${to}.`, run: () => km.channelSet(id, ch, to, cur) }
}

function confirmed() {
  const p = pending.value
  pending.value = null
  if (p) run(p.done, p.run)
}
</script>

<style scoped>
.desc { margin: 8px 0; }
.actions { margin-top: 6px; }
.wrap { word-break: break-all; }
.meter { display: inline-block; width: 70px; height: 8px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 4px; vertical-align: middle; overflow: hidden; }
.meter > span { display: block; height: 100%; background: var(--accent); }
.mv { margin-top: 10px; align-items: flex-end; }
.hist { list-style: none; font-size: 0.85rem; }
.hist li { padding: 2px 0; border-bottom: 1px solid var(--border); }
</style>

<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Data studio</h2>
      <label class="row">
        <span class="muted">data KM</span>
        <select v-model="current" aria-label="data KM" @change="pick">
          <option value="">choose...</option>
          <option v-for="k in kms" :key="k.id" :value="k.id">{{ k.name || k.id }} ({{ k.id }})</option>
        </select>
      </label>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>
    <p v-if="!kms.length && !error" class="muted">No data KMs are installed. A data KM holds training records; the factory builds them, or import one.</p>

    <template v-if="current && listing">
      <div class="card">
        <div class="row">
          <span class="pill approved">{{ listing.counts.approved }} approved</span>
          <span class="pill pending">{{ listing.counts.pending }} pending</span>
          <span class="pill rejected">{{ listing.counts.rejected }} rejected</span>
          <span class="muted small">{{ listing.counts.total }} records, working copy from {{ listing.base_version }}</span>
          <span v-if="listing.edited" class="pill pending">uncommitted edits</span>
          <span class="grow"></span>
          <input v-model="newVersion" type="text" placeholder="new version (optional)" aria-label="new version" style="width: 160px" />
          <button class="btn primary" :disabled="!listing.edited || busy" @click="confirmCommit = true">New version from edits</button>
        </div>
      </div>

      <form class="card" @submit.prevent="reload(true)">
        <div class="row">
          <input v-model="f.text" type="text" placeholder="search text in records" class="grow" aria-label="search text" />
          <select v-model="f.status" aria-label="status">
            <option value="">any status</option>
            <option v-for="s in ['pending', 'approved', 'rejected']" :key="s" :value="s">{{ s }}</option>
          </select>
          <select v-model="f.split" aria-label="split">
            <option value="">any split</option>
            <option v-for="s in ['train', 'valid', 'test']" :key="s" :value="s">{{ s }}</option>
          </select>
          <input v-model="f.tag" type="text" placeholder="tag e.g. lang:rust" style="width: 150px" aria-label="tag" />
          <input v-model="f.task" type="text" placeholder="task" style="width: 90px" aria-label="task" />
          <input v-model.number="f.quality" type="number" min="0" max="1" step="0.1" placeholder="min quality" style="width: 110px" aria-label="minimum quality" />
          <select v-model="f.source" aria-label="source kind">
            <option value="">any source</option>
            <option v-for="s in sourceKinds" :key="s" :value="s">{{ s }}</option>
          </select>
          <button class="btn primary">Filter</button>
        </div>
        <p class="muted small">Status, split, tag, task and text are applied by the hub; quality and source kind to the page of records shown.</p>
      </form>

      <div class="split">
        <div class="card">
          <div class="row bar">
            <strong>{{ visible.length }} shown</strong>
            <span class="muted small">of {{ listing.matching }} matching</span>
            <span class="grow"></span>
            <span class="muted small" v-if="picked.length">{{ picked.length }} selected</span>
            <button class="btn" :disabled="!picked.length || busy" @click="bulk('approve')">Approve selected</button>
            <button class="btn" :disabled="!picked.length || busy" @click="bulk('reject')">Reject selected</button>
            <button class="btn" :disabled="!listing.counts.pending || busy" @click="confirmAllPending = true">Approve all pending</button>
            <button class="btn" @click="startNew">New record</button>
          </div>
          <div class="tablewrap">
            <table class="t" aria-label="records">
              <thead>
                <tr>
                  <th><input type="checkbox" :checked="allPicked" aria-label="select all shown" @change="toggleAll" /></th>
                  <th>record</th><th>status</th><th>input / output</th><th class="num">q</th><th class="num">rev</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="x in visible" :key="x.id" :class="{ sel: x.id === openId }">
                  <td><input v-model="picked" type="checkbox" :value="x.id" :aria-label="`select ${x.id}`" /></td>
                  <td>
                    <button class="linkbtn mono" @click="open(x.id)">{{ x.id }}</button>
                    <div class="muted small">{{ x.split || '' }} {{ x.task || '' }}</div>
                  </td>
                  <td><span class="pill" :class="x.status">{{ x.status }}</span></td>
                  <td class="prev">
                    <div>{{ preview(lastUser(x)) }}</div>
                    <div class="muted">{{ preview(x.output) }}</div>
                    <span v-for="t in (x.tags || []).slice(0, 4)" :key="t" class="pill">{{ t }}</span>
                  </td>
                  <td class="num">{{ x.labels?.quality ?? '' }}</td>
                  <td class="num">{{ x.rev }}</td>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="!visible.length" class="muted">No records match.</p>
          <div class="row pager">
            <button class="btn" :disabled="offset === 0" @click="page(-1)">Previous</button>
            <span class="muted small">{{ listing.matching ? `${offset + 1}-${Math.min(offset + size, listing.matching)}` : '' }}</span>
            <button class="btn" :disabled="offset + size >= listing.matching" @click="page(1)">Next</button>
          </div>
        </div>

        <div class="card editor" v-if="draft" aria-label="record editor">
          <div class="row">
            <h3 class="grow">{{ isNew ? 'New record' : draft.id }}</h3>
            <span v-if="!isNew" class="pill" :class="draft.status">{{ draft.status }}</span>
            <span v-if="!isNew" class="muted small">rev {{ draft.rev }}<span v-if="draft.edited_by"> by {{ draft.edited_by }}</span></span>
            <button class="btn" aria-label="close editor" @click="close">Close</button>
          </div>
          <div class="tabs" role="tablist">
            <button v-for="t in ['edit', 'history', 'provenance']" :key="t" role="tab" :aria-selected="tab === t" :disabled="isNew && t !== 'edit'" @click="tab = t">{{ t }}</button>
          </div>

          <form v-if="tab === 'edit'" @submit.prevent="save">
            <label v-if="isNew" class="field">record id
              <input v-model="newId" type="text" required />
            </label>
            <fieldset class="msgs">
              <legend>Input messages</legend>
              <div v-for="(m, i) in draft.input" :key="i" class="msg">
                <div class="row">
                  <select v-model="m.role" :aria-label="`role of message ${i + 1}`">
                    <option v-for="r in ['system', 'user', 'assistant', 'tool']" :key="r" :value="r">{{ r }}</option>
                  </select>
                  <span class="grow"></span>
                  <button type="button" class="btn" :aria-label="`remove message ${i + 1}`" @click="draft.input.splice(i, 1)">Remove</button>
                </div>
                <textarea v-model="m.content" rows="3" :aria-label="`content of message ${i + 1}`"></textarea>
              </div>
              <button type="button" class="btn" @click="draft.input.push({ role: 'user', content: '' })">Add message</button>
            </fieldset>
            <label class="field">Output
              <textarea v-model="draft.output" rows="6"></textarea>
            </label>
            <div class="formgrid">
              <label class="field">Tags (comma separated)
                <input v-model="tagText" type="text" />
              </label>
              <label class="field">Split
                <select v-model="draft.split"><option value="">none</option><option v-for="s in ['train', 'valid', 'test']" :key="s" :value="s">{{ s }}</option></select>
              </label>
              <label class="field">Task
                <input v-model="draft.task" type="text" />
              </label>
            </div>
            <label class="field">Labels (JSON object)
              <textarea v-model="labelText" rows="3" class="mono"></textarea>
            </label>
            <div v-if="draft.media?.length" class="media">
              <strong class="small muted">Media</strong>
              <div v-for="(md, i) in draft.media" :key="i" class="medium">
                <img v-if="mediaUrl(md) && md.kind === 'image'" :src="mediaUrl(md)" :alt="`media ${i + 1}`" />
                <audio v-else-if="mediaUrl(md) && md.kind === 'audio'" :src="mediaUrl(md)" controls></audio>
                <div v-else class="ph" role="img" :aria-label="`${md.kind} file`">{{ md.kind }}</div>
                <div class="mono small muted">{{ md.path }}<br />{{ (md.sha256 || '').slice(0, 16) }}</div>
              </div>
              <p class="muted small">The hub serves no payload files yet, so media shows its details unless the record carries a url.</p>
            </div>
            <label class="field">Why (kept in the history)
              <input v-model="editNote" type="text" />
            </label>
            <p v-if="draftError" class="error-box" role="alert">{{ draftError }}</p>
            <div class="row">
              <button class="btn primary" :disabled="busy || !dirty">{{ isNew ? 'Create' : 'Save as rev ' + (draft.rev + 1) }}</button>
              <template v-if="!isNew">
                <button type="button" class="btn" :disabled="busy || draft.status === 'approved'" @click="one('approve')">Approve</button>
                <button type="button" class="btn" :disabled="busy || draft.status === 'rejected'" @click="one('reject')">Reject</button>
              </template>
              <span v-if="dirty" class="muted small">unsaved changes</span>
            </div>
          </form>

          <div v-else-if="tab === 'history'">
            <p v-if="!revs.length" class="muted">Loading...</p>
            <div v-else>
              <div class="row">
                <label class="field">from
                  <select v-model.number="revA"><option v-for="v in revs" :key="v.rev + v.at" :value="v.rev">rev {{ v.rev }}</option></select>
                </label>
                <label class="field">to
                  <select v-model.number="revB"><option v-for="v in revs" :key="v.rev + v.at" :value="v.rev">rev {{ v.rev }}</option></select>
                </label>
              </div>
              <div class="difftext card" aria-label="revision diff">
                <span v-for="(l, i) in revDiff" :key="i" :class="l.op">{{ (l.op === 'ins' ? '+ ' : l.op === 'del' ? '- ' : '  ') + l.text }}</span>
              </div>
              <ul class="revs">
                <li v-for="v in [...revs].reverse()" :key="v.rev + '-' + v.at">
                  <strong>rev {{ v.rev }}</strong> <span class="pill">{{ v.action }}</span>
                  <span class="pill" :class="v.record.status">{{ v.record.status }}</span>
                  <span class="muted small">{{ fmtWhen(v.at) }} by {{ v.by || '-' }}</span>
                  <span v-if="v.note" class="small"> &mdash; {{ v.note }}</span>
                </li>
              </ul>
            </div>
          </div>

          <dl v-else class="kv">
            <dt>source kind</dt><dd>{{ draft.source?.kind || '-' }}</dd>
            <dt>teacher model</dt><dd class="mono">{{ draft.source?.model || '-' }}</dd>
            <dt>trace</dt>
            <dd>
              <router-link v-if="draft.source?.trace" :to="`/traces/${draft.source.trace}`" class="mono">{{ draft.source.trace }}</router-link>
              <span v-else>-</span>
            </dd>
            <template v-for="(v, k) in otherSource" :key="k"><dt>{{ k }}</dt><dd>{{ typeof v === 'object' ? JSON.stringify(v) : v }}</dd></template>
            <dt>task</dt>
            <dd>
              <router-link v-if="draft.task" :to="{ path: '/traces', query: { task: draft.task } }">{{ draft.task }} (traces)</router-link>
              <span v-else>-</span>
            </dd>
            <dt>last edited by</dt><dd>{{ draft.edited_by || '-' }}</dd>
            <dt>revision</dt><dd>{{ draft.rev }}</dd>
          </dl>
        </div>
        <div v-else class="card muted">Pick a record to read or edit it.</div>
      </div>
    </template>

    <ConfirmDialog :open="confirmCommit" title="Create a new version?" action="Create version" @confirm="commit" @cancel="confirmCommit = false">
      The edits become a new version of {{ current }} whose parent is the version they started from. The old version is untouched.
    </ConfirmDialog>
    <ConfirmDialog :open="confirmAllPending" title="Approve every pending record?" action="Approve all" @confirm="approveAllPending" @cancel="confirmAllPending = false">
      All {{ listing?.counts.pending }} pending records of {{ current }} become approved, each as a new revision.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { km, type DataRecord, type KmItem, type Revision } from '@/api'
import { diffLines, fmtWhen, recordText, splitList } from '@/util'

const props = defineProps<{ id?: string }>()
const router = useRouter()
const kms = ref<KmItem[]>([])
const current = ref(props.id || '')
const listing = ref<any>(null)
const error = ref('')
const note = ref('')
const busy = ref(false)
const size = 50
const offset = ref(0)
const f = reactive({ text: '', status: '', split: '', tag: '', task: '', quality: null as number | null, source: '' })
const picked = ref<string[]>([])
const openId = ref('')
const draft = ref<any>(null)
const original = ref('')
const isNew = ref(false)
const newId = ref('')
const tab = ref('edit')
const tagText = ref('')
const labelText = ref('')
const editNote = ref('')
const draftError = ref('')
const revs = ref<Revision[]>([])
const revA = ref(0)
const revB = ref(0)
const newVersion = ref('')
const confirmCommit = ref(false)
const confirmAllPending = ref(false)

const rows = computed<DataRecord[]>(() => listing.value?.records || [])
const sourceKinds = computed(() => [...new Set(rows.value.map((r) => r.source?.kind).filter(Boolean))] as string[])
const visible = computed(() =>
  rows.value.filter(
    (r) =>
      (f.quality == null || (Number(r.labels?.quality) || 0) >= f.quality) && (!f.source || r.source?.kind === f.source),
  ),
)
const allPicked = computed(() => visible.value.length > 0 && visible.value.every((r) => picked.value.includes(r.id)))
const otherSource = computed(() => Object.fromEntries(Object.entries(draft.value?.source || {}).filter(([k]) => !['kind', 'model', 'trace'].includes(k))))
const lastUser = (r: DataRecord) => [...(r.input || [])].reverse().find((m) => m.role === 'user')?.content || r.input?.[0]?.content || ''
const preview = (s?: string) => (s && s.length > 120 ? s.slice(0, 120) + '...' : s || '')

const assembled = (): Record<string, unknown> => {
  const d = draft.value
  let labels: unknown = {}
  try {
    labels = labelText.value.trim() ? JSON.parse(labelText.value) : {}
  } catch (_) {
    labels = undefined
  }
  return { input: d.input, output: d.output, tags: splitList(tagText.value), split: d.split || undefined, task: d.task || undefined, labels }
}
const dirty = computed(() => (draft.value ? isNew.value || JSON.stringify(assembled()) !== original.value : false))

const mediaUrl = (m: any): string => (typeof m.url === 'string' && /^(https?:|data:image|data:audio|\/)/.test(m.url) ? m.url : '')

async function loadKms() {
  try {
    const all = await km.list({ kind: 'data' })
    const seen = new Map<string, KmItem>()
    for (const k of all) if (!seen.has(k.id)) seen.set(k.id, k)
    kms.value = [...seen.values()]
    if (current.value) await reload()
  } catch (e: any) {
    error.value = e.message
  }
}

async function reload(reset = false) {
  if (!current.value) return
  if (reset) offset.value = 0
  error.value = ''
  try {
    listing.value = await km.data.list(current.value, {
      limit: size, offset: offset.value,
      ...Object.fromEntries(Object.entries({ status: f.status, split: f.split, tag: f.tag, task: f.task, text: f.text }).filter(([, v]) => v)),
    })
    picked.value = picked.value.filter((p) => rows.value.some((r) => r.id === p))
  } catch (e: any) {
    error.value = e.message
    listing.value = null
  }
}
function pick() {
  close()
  picked.value = []
  router.replace(current.value ? `/data/${encodeURIComponent(current.value)}` : '/data')
  reload(true)
}
const page = (d: number) => {
  offset.value = Math.max(0, offset.value + d * size)
  reload()
}
const toggleAll = () => {
  picked.value = allPicked.value ? [] : visible.value.map((r) => r.id)
}

function setDraft(r: any) {
  const c = JSON.parse(JSON.stringify(r))
  c.input = c.input || []
  c.output = c.output ?? ''
  c.split = c.split || ''
  draft.value = c
  tagText.value = (c.tags || []).join(', ')
  labelText.value = JSON.stringify(c.labels || {}, null, 2)
  original.value = JSON.stringify(assembled())
  editNote.value = ''
  draftError.value = ''
}
async function open(id: string) {
  isNew.value = false
  openId.value = id
  tab.value = 'edit'
  try {
    const r = await km.data.get(current.value, id, true)
    revs.value = r.history || []
    revB.value = r.rev
    revA.value = Math.max(1, r.rev - 1)
    setDraft(r)
  } catch (e: any) {
    error.value = e.message
  }
}
function startNew() {
  isNew.value = true
  openId.value = ''
  tab.value = 'edit'
  newId.value = `r-${Date.now().toString(36)}`
  setDraft({ status: 'pending', input: [{ role: 'user', content: '' }], output: '', tags: [], labels: {} })
}
function close() {
  draft.value = null
  openId.value = ''
  isNew.value = false
}

const revDiff = computed(() => {
  const a = revs.value.find((v) => v.rev === revA.value)
  const b = revs.value.find((v) => v.rev === revB.value)
  return diffLines(recordText(a?.record), recordText(b?.record))
})

async function save() {
  draftError.value = ''
  const c = assembled()
  if (c.labels === undefined) {
    draftError.value = 'Labels must be a JSON object.'
    return
  }
  busy.value = true
  try {
    const id = isNew.value ? newId.value.trim() : draft.value.id
    if (!id) throw new Error('A record needs an id.')
    const changes = Object.fromEntries(Object.entries(c).filter(([, v]) => v !== undefined))
    await km.data.edit(current.value, id, changes, {
      note: editNote.value || undefined,
      ...(isNew.value ? { create: true } : { expect_rev: draft.value.rev }),
    })
    note.value = isNew.value ? `Created ${id}.` : `Saved ${id} as a new revision.`
    await reload()
    await open(id)
  } catch (e: any) {
    draftError.value = e.message
  } finally {
    busy.value = false
  }
}

async function one(what: 'approve' | 'reject') {
  await decide(what, [draft.value.id])
  await open(draft.value.id)
}
async function bulk(what: 'approve' | 'reject') {
  await decide(what, picked.value)
  picked.value = []
}
async function decide(what: 'approve' | 'reject', ids: string[]) {
  busy.value = true
  error.value = ''
  try {
    const r = await (what === 'approve' ? km.data.approve : km.data.reject)(current.value, ids)
    note.value = `${r.changed?.length ?? ids.length} record(s) ${what === 'approve' ? 'approved' : 'rejected'}.`
    await reload()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
async function approveAllPending() {
  confirmAllPending.value = false
  busy.value = true
  try {
    const r = await km.data.approveFrom(current.value, 'pending')
    note.value = `${r.changed?.length ?? 0} record(s) approved.`
    await reload()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
async function commit() {
  confirmCommit.value = false
  busy.value = true
  error.value = ''
  try {
    const r = await km.data.commit(current.value, newVersion.value.trim() || undefined)
    note.value = `Committed ${r.committed}${r.parent ? ` (parent ${r.parent})` : ''}.`
    newVersion.value = ''
    await reload()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}

watch(() => props.id, (v) => {
  if (v !== current.value) {
    current.value = v || ''
    close()
    reload(true)
  }
})
onMounted(loadKms)
</script>

<style scoped>
.linkbtn { background: none; border: 0; color: var(--accent); padding: 0; text-align: left; text-decoration: underline; }
.prev { max-width: 380px; font-size: 0.85rem; overflow-wrap: anywhere; }
.msgs { border: 1px solid var(--border); border-radius: 8px; padding: 8px; margin-bottom: 10px; }
.msgs legend { padding: 0 6px; color: var(--muted); font-size: 0.85rem; }
.msg { margin-bottom: 8px; }
.field { margin-bottom: 10px; }
.media { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 10px; align-items: flex-start; }
.medium img { max-width: 160px; max-height: 120px; border-radius: 6px; border: 1px solid var(--border); }
.ph { width: 120px; height: 80px; display: flex; align-items: center; justify-content: center; background: var(--panel-2); border: 1px dashed var(--border); border-radius: 6px; color: var(--muted); }
.revs { list-style: none; font-size: 0.85rem; }
.revs li { padding: 3px 0; border-bottom: 1px solid var(--border); }
.editor { position: sticky; top: 8px; max-height: calc(100vh - 80px); overflow-y: auto; }
.pager { justify-content: center; margin-top: 8px; }
@media (max-width: 800px) { .editor { position: static; max-height: none; } }
</style>

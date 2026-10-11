<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/factory" class="btn">Back</router-link>
      <h2 class="grow">Review queue</h2>
      <select v-model="kind" aria-label="what to review" @change="load">
        <option value="">everything</option>
        <option value="record">records</option>
        <option value="tag">tags</option>
        <option value="vocab">new vocabulary values</option>
        <option value="promotion">promotions</option>
      </select>
      <input v-model="task" type="text" placeholder="task id" aria-label="task id" style="width: 130px" @change="load" />
      <label class="row small"><input v-model="decided" type="checkbox" @change="load" /> include decided</label>
      <button class="btn" @click="load">Refresh</button>
    </div>
    <Unavailable v-if="gone" what="Factory review queue" op="factory.review.list" />
    <p v-else-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <template v-if="!gone">
      <p class="muted small">
        Keyboard (when the list has focus): <kbd>j</kbd>/<kbd>k</kbd> next and previous, <kbd>a</kbd> approve, <kbd>e</kbd> edit a record,
        <kbd>r</kbd> reject, <kbd>Ctrl</kbd>+<kbd>Enter</kbd> save an edit, <kbd>Esc</kbd> cancel it.
        {{ total }} item(s){{ decided ? '' : ' open' }}.
      </p>
      <div class="split">
        <div
          ref="listEl" class="card queue" tabindex="0" role="listbox" aria-label="items awaiting a decision"
          :aria-activedescendant="cur ? 'it-' + cur.id : undefined" @keydown="onKey"
        >
          <div
            v-for="(it, i) in items" :id="'it-' + it.id" :key="it.id" role="option" :aria-selected="i === idx"
            class="item" :class="{ on: i === idx }" @click="idx = i"
          >
            <div class="row">
              <span class="pill">{{ it.type }}</span>
              <strong v-if="it.type === 'tag' || it.type === 'vocab'" class="mono">{{ it.tag }}</strong>
              <span v-else-if="it.type === 'promotion'" class="mono">{{ (it.data?.kms || []).join(', ') }}</span>
              <span v-else class="mono">{{ it.record_id || it.id }}</span>
              <span v-if="it.confidence != null" class="muted small">conf {{ Number(it.confidence).toFixed(2) }}</span>
              <span v-if="it.state !== 'open'" class="pill" :class="it.state">{{ it.state }}</span>
              <span class="grow"></span>
              <span v-if="it.task" class="muted small">task {{ it.task }}</span>
            </div>
            <div class="muted small">{{ it.reason || '' }}</div>
          </div>
          <p v-if="!items.length && loaded" class="muted">Nothing {{ decided ? 'in the queue' : 'awaits a decision' }}.</p>
        </div>

        <div v-if="cur" class="card">
          <div class="row">
            <h3 class="grow">{{ titles[cur.type] || cur.type }} {{ cur.type === 'record' ? cur.record_id : cur.type === 'promotion' ? '' : cur.tag }}</h3>
            <span class="muted small">{{ idx + 1 }} of {{ items.length }}</span>
          </div>
          <dl class="kv">
            <dt>item</dt><dd class="mono">{{ cur.id }}</dd>
            <dt v-if="cur.task">task</dt><dd v-if="cur.task"><router-link :to="`/factory/${encodeURIComponent(cur.task)}`">{{ cur.task }}</router-link></dd>
            <dt v-if="cur.origin">origin</dt><dd v-if="cur.origin">{{ typeof cur.origin === 'string' ? cur.origin : JSON.stringify(cur.origin) }}</dd>
            <dt v-if="cur.reason">why it is here</dt><dd v-if="cur.reason">{{ cur.reason }}</dd>
            <template v-if="cur.state !== 'open'">
              <dt>decision</dt><dd>{{ cur.decision }} by {{ cur.decided_by || '-' }}<span v-if="cur.note">: {{ cur.note }}</span></dd>
            </template>
          </dl>

          <template v-if="cur.type === 'record'">
            <template v-if="!editing">
              <pre v-if="cur.record" class="mono rec">{{ JSON.stringify(cur.record, null, 2) }}</pre>
            </template>
            <template v-else>
              <label class="field">Input (the user message)
                <textarea ref="editEl" v-model="edit.input" rows="4" @keydown.ctrl.enter.prevent="decide('edit')" @keydown.esc.stop.prevent="editing = false"></textarea>
              </label>
              <label class="field">Output
                <textarea v-model="edit.output" rows="8" @keydown.ctrl.enter.prevent="decide('edit')" @keydown.esc.stop.prevent="editing = false"></textarea>
              </label>
              <label class="field">Tags (comma separated; empty leaves them as they are)
                <input v-model="edit.tags" type="text" @keydown.ctrl.enter.prevent="decide('edit')" @keydown.esc.stop.prevent="editing = false" />
              </label>
            </template>
          </template>

          <template v-else-if="cur.type === 'tag'">
            <p>
              Proposed tag <span class="pill">{{ cur.tag }}</span> for record
              <span class="mono">{{ cur.record_id }}</span><span v-if="cur.data?.confidence != null"> (confidence {{ Number(cur.data.confidence).toFixed(2) }})</span>
            </p>
            <pre v-if="cur.data && Object.keys(cur.data).length" class="mono rec">{{ JSON.stringify(cur.data, null, 2) }}</pre>
            <label v-if="cur.state === 'open'" class="row small"><input v-model="addVocab" type="checkbox" /> If the tag is outside the vocabulary, add its value on approval</label>
          </template>

          <template v-else-if="cur.type === 'vocab'">
            <p>The author model proposes a new value <span class="pill">{{ cur.tag }}</span> for the vocabulary. Approving adds it; rejecting leaves it out.</p>
            <template v-if="cur.record_id">
              <p class="muted small">An example record that uses it</p>
              <pre class="mono rec">{{ typeof cur.record_id === 'string' ? cur.record_id : JSON.stringify(cur.record_id, null, 2) }}</pre>
            </template>
            <pre v-if="cur.data && Object.keys(cur.data).length" class="mono rec">{{ JSON.stringify(cur.data, null, 2) }}</pre>
          </template>

          <template v-else-if="cur.type === 'promotion'">
            <p>The task's KMs are candidates for the <span class="pill">stable</span> channel (policy: propose). Approving moves stable to them (adapters stay).</p>
            <ul class="plain">
              <li v-for="k in cur.data?.kms || []" :key="k">
                <router-link :to="`/knowledge/${encodeURIComponent(String(k).split('@')[0])}`" class="mono">{{ k }}</router-link>
              </li>
            </ul>
            <p v-if="cur.data?.uplift != null">Measured uplift <strong>{{ Number(cur.data.uplift).toFixed(3) }}</strong></p>
            <p v-else class="muted">No uplift was measured.</p>
          </template>

          <pre v-else class="mono rec">{{ JSON.stringify(cur.data, null, 2) }}</pre>

          <p v-if="decideError" class="error-box" role="alert">{{ decideError }}</p>
          <div v-if="cur.state === 'open'" class="row">
            <template v-if="!editing">
              <button class="btn primary" :disabled="busy" @click="decide('approve')">{{ cur.type === 'promotion' ? 'Promote' : 'Approve' }} (a)</button>
              <button v-if="cur.type === 'record'" class="btn" :disabled="busy" @click="startEdit">Edit (e)</button>
              <button class="btn danger" :disabled="busy" @click="decide('reject')">Reject (r)</button>
            </template>
            <template v-else>
              <button class="btn primary" :disabled="busy" @click="decide('edit')">Save edit and approve</button>
              <button class="btn" @click="editing = false">Cancel</button>
            </template>
          </div>
        </div>
        <div v-else class="card muted">{{ items.length ? 'Select an item.' : '' }}</div>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref } from 'vue'
import { useRoute } from 'vue-router'
import Unavailable from '@/components/Unavailable.vue'
import { factory, unavailable, type ReviewItem } from '@/api'

// factory.review.list answers {total, items}; items: {id (a number), type: record | tag | vocab | promotion, task, record_id,
// record, tag, confidence, origin, reason, data, state, decision, decided_by}. The queue holds open items unless state=all.
// factory.review.decide {item, decision: approve | edit | reject, changes?: {input, output, tags}, add_to_vocabulary?}.
const titles: Record<string, string> = { record: 'Record', tag: 'Tag', vocab: 'New vocabulary value', promotion: 'Promotion' }
const route = useRoute()
const items = ref<ReviewItem[]>([])
const total = ref(0)
const idx = ref(0)
const kind = ref('')
const task = ref((route.query.task as string) || '')
const decided = ref(false)
const addVocab = ref(true)
const gone = ref(false)
const loaded = ref(false)
const error = ref('')
const note = ref('')
const busy = ref(false)
const editing = ref(false)
const edit = reactive({ input: '', output: '', tags: '' })
const decideError = ref('')
const listEl = ref<HTMLElement | null>(null)
const editEl = ref<HTMLTextAreaElement | null>(null)
const cur = computed(() => items.value[idx.value])

async function load() {
  try {
    const r = await factory.reviewList({
      limit: 200, ...(decided.value ? { state: 'all' } : {}),
      ...(kind.value ? { kind: kind.value } : {}), ...(task.value ? { task: task.value } : {}),
    })
    items.value = r.items
    total.value = r.total
    idx.value = Math.min(idx.value, Math.max(0, items.value.length - 1))
    error.value = ''
    loaded.value = true
  } catch (e: any) {
    if (unavailable(e)) gone.value = true
    else error.value = e.message
  }
}

const textOf = (c: any): string => (Array.isArray(c) ? [...c].reverse().find((m: any) => m.role === 'user')?.content || c[0]?.content || '' : c || '')
function startEdit() {
  const it = cur.value
  if (!it || it.type !== 'record' || it.state !== 'open') return
  editing.value = true
  decideError.value = ''
  edit.input = textOf(it.record?.input)
  edit.output = it.record?.output ?? ''
  edit.tags = ''
  nextTick(() => editEl.value?.focus())
}

const label = (it: ReviewItem) => (it.type === 'record' ? it.record_id : it.type === 'promotion' ? 'the promotion' : it.tag) || it.id
async function decide(d: 'approve' | 'edit' | 'reject') {
  const it = cur.value
  if (!it || busy.value || it.state !== 'open') return
  if (d === 'edit' && it.type !== 'record') return
  decideError.value = ''
  const extra: Record<string, unknown> = {}
  if (d === 'edit') {
    const changes: Record<string, unknown> = { input: edit.input, output: edit.output }
    const tags = edit.tags.split(',').map((x) => x.trim()).filter(Boolean)
    if (tags.length) changes.tags = tags
    extra.changes = changes
  }
  if (it.type === 'tag' && d === 'approve') extra.add_to_vocabulary = addVocab.value
  busy.value = true
  try {
    const r = await factory.decide(it.id, d, extra)
    note.value = `${d === 'approve' ? (it.type === 'promotion' ? 'Promoted' : 'Approved') : d === 'edit' ? 'Edited and approved' : 'Rejected'} ${label(it)}.`
    if (r?.effect?.promoted?.length) note.value += ` stable now points at ${r.effect.promoted.join(', ')}.`
    if (r?.effect?.added) note.value += ` Added ${r.effect.added} to the vocabulary.`
    editing.value = false
    if (decided.value) await load()
    else {
      items.value = items.value.filter((x) => x.id !== it.id) // the next item takes this place
      total.value = Math.max(0, total.value - 1)
      idx.value = Math.min(idx.value, Math.max(0, items.value.length - 1))
    }
    listEl.value?.focus()
  } catch (e: any) {
    decideError.value = e.message
  } finally {
    busy.value = false
  }
}

function onKey(e: KeyboardEvent) {
  if (e.ctrlKey || e.metaKey || e.altKey || editing.value) return
  const k = e.key.toLowerCase()
  const act: Record<string, () => void> = {
    j: () => (idx.value = Math.min(idx.value + 1, items.value.length - 1)),
    arrowdown: () => (idx.value = Math.min(idx.value + 1, items.value.length - 1)),
    k: () => (idx.value = Math.max(idx.value - 1, 0)),
    arrowup: () => (idx.value = Math.max(idx.value - 1, 0)),
    a: () => decide('approve'),
    r: () => decide('reject'),
    e: startEdit,
  }
  if (act[k]) {
    e.preventDefault()
    act[k]()
  }
}
onMounted(load)
</script>

<style scoped>
.queue { max-height: 75vh; overflow-y: auto; padding: 6px; }
.item { padding: 8px; border-radius: 8px; cursor: pointer; border: 1px solid transparent; }
.item.on { background: var(--panel-2); border-color: var(--accent); }
.rec { overflow: auto; max-height: 340px; padding: 8px; background: var(--panel-2); border-radius: 8px; margin: 8px 0; white-space: pre-wrap; }
.plain { list-style: none; margin-bottom: 8px; }
kbd { font-family: var(--mono); border: 1px solid var(--border); border-bottom-width: 2px; border-radius: 4px; padding: 0 5px; background: var(--panel-2); }
</style>

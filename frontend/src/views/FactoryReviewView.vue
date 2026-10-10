<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/factory" class="btn">Back</router-link>
      <h2 class="grow">Review queue</h2>
      <select v-model="kind" aria-label="what to review" @change="load">
        <option value="">records and tags</option>
        <option value="record">records</option>
        <option value="tag">tags</option>
      </select>
      <input v-model="task" type="text" placeholder="task id" aria-label="task id" style="width: 130px" @change="load" />
      <button class="btn" @click="load">Refresh</button>
    </div>
    <Unavailable v-if="gone" what="Factory review queue" op="factory.review.list" />
    <p v-else-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <template v-if="!gone">
      <p class="muted small">
        Keyboard (when the list has focus): <kbd>j</kbd>/<kbd>k</kbd> next and previous, <kbd>a</kbd> approve, <kbd>e</kbd> edit,
        <kbd>r</kbd> reject, <kbd>Ctrl</kbd>+<kbd>Enter</kbd> save an edit, <kbd>Esc</kbd> cancel it.
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
              <strong v-if="it.type === 'tag'" class="mono">{{ it.tag }}</strong>
              <span v-else class="mono">{{ it.record_id || it.record?.id || it.id }}</span>
              <span v-if="it.confidence != null" class="muted small">conf {{ Number(it.confidence).toFixed(2) }}</span>
              <span class="grow"></span>
              <span v-if="it.task" class="muted small">task {{ it.task }}</span>
            </div>
            <div class="muted small">{{ it.reason || '' }}</div>
          </div>
          <p v-if="!items.length && loaded" class="muted">Nothing awaits a decision.</p>
        </div>

        <div class="card" v-if="cur">
          <div class="row">
            <h3 class="grow">{{ cur.type === 'tag' ? 'Tag' : 'Record' }} {{ cur.type === 'tag' ? cur.tag : cur.record_id || cur.id }}</h3>
            <span class="muted small">{{ idx + 1 }} of {{ items.length }}</span>
          </div>
          <dl class="kv">
            <dt v-if="cur.km">KM</dt><dd v-if="cur.km"><router-link :to="`/data/${encodeURIComponent(cur.km)}`">{{ cur.km }}</router-link></dd>
            <dt v-if="cur.task">task</dt><dd v-if="cur.task"><router-link :to="`/factory/${encodeURIComponent(cur.task)}`">{{ cur.task }}</router-link></dd>
            <dt v-if="cur.origin">origin</dt><dd v-if="cur.origin">{{ cur.origin }}</dd>
            <dt v-if="cur.reason">why it is here</dt><dd v-if="cur.reason">{{ cur.reason }}</dd>
          </dl>
          <template v-if="!editing">
            <pre v-if="cur.record" class="mono rec">{{ JSON.stringify(cur.record, null, 2) }}</pre>
            <div v-if="cur.type === 'tag'">
              <p>Proposed tag <span class="pill">{{ cur.tag }}</span><span v-if="cur.for"> for {{ cur.for }}</span></p>
              <pre v-if="cur.context" class="mono rec">{{ typeof cur.context === 'string' ? cur.context : JSON.stringify(cur.context, null, 2) }}</pre>
            </div>
          </template>
          <template v-else>
            <label class="field">{{ cur.type === 'tag' ? 'Replace the tag with' : 'Record (JSON)' }}
              <textarea ref="editEl" v-model="editText" :rows="cur.type === 'tag' ? 1 : 14" class="mono" spellcheck="false" @keydown.ctrl.enter.prevent="decide('edit')" @keydown.esc.stop.prevent="editing = false"></textarea>
            </label>
          </template>
          <p v-if="decideError" class="error-box" role="alert">{{ decideError }}</p>
          <div class="row">
            <template v-if="!editing">
              <button class="btn primary" :disabled="busy" @click="decide('approve')">Approve (a)</button>
              <button class="btn" :disabled="busy" @click="startEdit">Edit (e)</button>
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
import { computed, nextTick, onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'
import Unavailable from '@/components/Unavailable.vue'
import { factory, unavailable } from '@/api'

// factory.review.list items: {id, type: 'record' | 'tag', task, km, record_id, record, tag, confidence, origin, reason}.
// factory.review.decide {item, decision: 'approve' | 'edit' | 'reject', record? | tag?}.
const route = useRoute()
const items = ref<any[]>([])
const idx = ref(0)
const kind = ref('')
const task = ref((route.query.task as string) || '')
const gone = ref(false)
const loaded = ref(false)
const error = ref('')
const note = ref('')
const busy = ref(false)
const editing = ref(false)
const editText = ref('')
const decideError = ref('')
const listEl = ref<HTMLElement | null>(null)
const editEl = ref<HTMLTextAreaElement | null>(null)
const cur = computed(() => items.value[idx.value])

async function load() {
  try {
    items.value = await factory.reviewList({ ...(kind.value ? { kind: kind.value } : {}), ...(task.value ? { task: task.value } : {}) })
    idx.value = Math.min(idx.value, Math.max(0, items.value.length - 1))
    error.value = ''
    loaded.value = true
  } catch (e: any) {
    if (unavailable(e)) gone.value = true
    else error.value = e.message
  }
}

function startEdit() {
  if (!cur.value) return
  editing.value = true
  decideError.value = ''
  editText.value = cur.value.type === 'tag' ? cur.value.tag : JSON.stringify(cur.value.record ?? {}, null, 2)
  nextTick(() => editEl.value?.focus())
}

async function decide(d: 'approve' | 'edit' | 'reject') {
  const it = cur.value
  if (!it || busy.value) return
  decideError.value = ''
  const extra: Record<string, unknown> = {}
  if (d === 'edit') {
    if (it.type === 'tag') extra.tag = editText.value.trim()
    else {
      try {
        extra.record = JSON.parse(editText.value)
      } catch (e: any) {
        decideError.value = `The record is not valid JSON: ${e.message}`
        return
      }
    }
  }
  busy.value = true
  try {
    await factory.decide(it.id, d, extra)
    note.value = `${d === 'approve' ? 'Approved' : d === 'edit' ? 'Edited and approved' : 'Rejected'} ${it.type === 'tag' ? it.tag : it.record_id || it.id}.`
    editing.value = false
    items.value = items.value.filter((x) => x.id !== it.id) // the next item takes this place
    idx.value = Math.min(idx.value, Math.max(0, items.value.length - 1))
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
kbd { font-family: var(--mono); border: 1px solid var(--border); border-bottom-width: 2px; border-radius: 4px; padding: 0 5px; background: var(--panel-2); }
</style>

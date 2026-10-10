<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Vocabulary</h2>
      <button class="btn" @click="load">Refresh</button>
    </div>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>
    <p class="muted">
      Tags are <span class="mono">namespace:value</span>. A closed namespace only accepts approved values; an open one accepts any.
      A KM with an unapproved tag is still installed; it is listed under Proposed until you approve the value.
    </p>

    <div v-if="pending.length" class="card">
      <h3>Proposed tags awaiting approval ({{ pending.length }})</h3>
      <table class="t" aria-label="proposed tags">
        <thead><tr><th>tag</th><th>why it is pending</th><th>used by</th><th></th></tr></thead>
        <tbody>
          <tr v-for="p in pending" :key="p.tag">
            <td class="mono">{{ p.tag }}</td>
            <td class="muted">{{ p.why }}</td>
            <td><router-link v-for="k in p.kms" :key="k" :to="`/knowledge/${encodeURIComponent(k)}`" class="pill">{{ k }}</router-link></td>
            <td><button class="btn primary" :disabled="busy" @click="approve(p.tag)">Approve</button></td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-else-if="v" class="card"><h3>Proposed tags</h3><p class="muted">Every tag on an installed KM is approved.</p></div>

    <div v-if="v" class="grid">
      <div v-for="(n, name) in v.namespaces" :key="name" class="card ns">
        <div class="row">
          <strong class="mono grow">{{ name }}:</strong>
          <label class="row small">
            <input type="checkbox" :checked="n.open" :disabled="busy" @change="edit({ namespace: String(name), open: ($event.target as HTMLInputElement).checked })" />
            open
          </label>
          <button class="btn danger" :disabled="busy" :aria-label="`delete namespace ${name}`" @click="del = String(name)">Delete</button>
        </div>
        <p class="muted small">{{ n.description || 'no description' }}</p>
        <div class="vals">
          <span v-for="val in n.values" :key="val" class="pill val">
            {{ val }}
            <button class="x" :aria-label="`remove ${name}:${val}`" :disabled="busy" @click="edit({ namespace: String(name), remove: [val] })">&times;</button>
          </span>
          <span v-if="!n.values.length" class="muted small">no approved values{{ n.open ? '' : ' (nothing is accepted)' }}</span>
        </div>
        <form class="row" @submit.prevent="addValue(String(name))">
          <input v-model="adding[name]" type="text" :aria-label="`new value for ${name}`" placeholder="new value" style="width: 150px" />
          <button class="btn" :disabled="busy || !adding[name]">Add</button>
        </form>
      </div>
    </div>

    <form class="card" @submit.prevent="createNs">
      <h3>New namespace</h3>
      <div class="row">
        <input v-model="nn.name" type="text" placeholder="name, e.g. domain" aria-label="namespace name" style="width: 160px" required />
        <input v-model="nn.description" type="text" placeholder="description" aria-label="description" class="grow" />
        <label class="row small"><input v-model="nn.open" type="checkbox" /> open</label>
        <button class="btn primary" :disabled="busy">Create</button>
      </div>
    </form>

    <ConfirmDialog :open="!!del" :title="`Delete ${del}:?`" action="Delete" @confirm="doDelete" @cancel="del = ''">
      The namespace and its approved values are removed. Tags that use it become unapproved.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { km, type KmItem, type Vocabulary } from '@/api'

const v = ref<Vocabulary | null>(null)
const kms = ref<KmItem[]>([])
const error = ref('')
const note = ref('')
const busy = ref(false)
const del = ref('')
const adding = reactive<Record<string, string>>({})
const nn = reactive({ name: '', description: '', open: false })

// Same rule as the hub's Vocabulary::problem, applied to every tag of every installed KM.
const pending = computed(() => {
  const out = new Map<string, { tag: string; why: string; kms: Set<string> }>()
  for (const k of kms.value)
    for (const tag of k.tags) {
      const [ns, val] = [tag.split(':')[0], tag.split(':').slice(1).join(':')]
      const n = v.value?.namespaces[ns]
      let why = ''
      if (!tag.includes(':')) why = 'not namespace:value'
      else if (!n) why = `namespace ${ns}: is not in the vocabulary`
      else if (!n.open && !n.values.includes(val)) why = `${val} is not an approved value of ${ns}:`
      if (!why) continue
      const e = out.get(tag) || { tag, why, kms: new Set<string>() }
      e.kms.add(k.id)
      out.set(tag, e)
    }
  return [...out.values()].map((e) => ({ ...e, kms: [...e.kms] }))
})

async function load() {
  error.value = ''
  try {
    const [vo, list] = await Promise.all([km.vocabularyGet(), km.list()])
    v.value = vo
    kms.value = list
  } catch (e: any) {
    error.value = e.message
  }
}
async function edit(a: Record<string, unknown>, msg = 'Saved.') {
  busy.value = true
  error.value = ''
  try {
    await km.vocabularyEdit(a)
    note.value = msg
    await load()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
const addValue = async (ns: string) => {
  await edit({ namespace: ns, add: [adding[ns].trim()] }, `Approved ${ns}:${adding[ns].trim()}.`)
  adding[ns] = ''
}
const approve = (tag: string) => {
  const [ns, ...rest] = tag.split(':')
  if (!rest.length) return (error.value = `${tag} is not namespace:value, so it cannot be approved.`)
  return edit({ namespace: ns, add: [rest.join(':')] }, `Approved ${tag}.`)
}
const createNs = async () => {
  await edit({ namespace: nn.name.trim(), description: nn.description, open: nn.open }, `Created ${nn.name.trim()}:.`)
  if (!error.value) Object.assign(nn, { name: '', description: '', open: false })
}
const doDelete = async () => {
  const n = del.value
  del.value = ''
  await edit({ namespace: n, delete: true }, `Deleted ${n}:.`)
}
onMounted(load)
</script>

<style scoped>
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 12px; margin: 14px 0; }
.ns { margin-bottom: 0; }
.vals { display: flex; flex-wrap: wrap; gap: 4px; margin: 8px 0; }
.val { display: inline-flex; align-items: center; gap: 4px; }
.x { background: none; border: 0; color: var(--muted); padding: 0 2px; line-height: 1; }
.x:hover { color: var(--bad); }
</style>

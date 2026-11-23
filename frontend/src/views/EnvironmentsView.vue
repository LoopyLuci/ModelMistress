<template>
  <div class="page">
    <div class="row bar">
      <h2 class="grow">Environments</h2>
      <router-link class="btn" to="/misty">Back to Misty</router-link>
    </div>
    <p class="muted">
      An environment is a place Misty works in: a working folder, the other folders she may reach, variables for the commands she runs, and
      what she may do there without asking. Pick one when you start a Misty chat. Only you can change them; she cannot.
    </p>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <div class="layout">
      <aside class="card list" aria-label="environments">
        <div class="row">
          <h3 class="grow">Yours</h3>
          <button class="btn" type="button" @click="ask(() => startNew())">New</button>
        </div>
        <ul>
          <li v-for="e in envs" :key="e.id" :class="{ on: e.id === draft.id }">
            <button type="button" class="link" @click="ask(() => select(e))">
              <span class="t">{{ e.name }}</span>
              <span class="muted small mono">{{ e.cwd || "Misty's own folder" }}</span>
            </button>
          </li>
        </ul>
        <p v-if="loaded && !envs.length" class="muted small">None yet. Make one, for example one for each project.</p>
      </aside>

      <section class="card edit" aria-label="environment">
        <h3>{{ draft.id ? 'Change this environment' : 'New environment' }}</h3>

        <label class="field">Name
          <input v-model="draft.name" type="text" maxlength="60" placeholder="For example: my app" />
        </label>
        <label class="field">What it is for (optional)
          <textarea v-model="draft.description" rows="2" maxlength="500"></textarea>
        </label>

        <div class="field">
          <span>Working folder</span>
          <div class="row">
            <input v-model="draft.cwd" type="text" class="grow mono" spellcheck="false" aria-label="Working folder" placeholder="Misty's own folder (leave empty), or a full path" />
            <button class="btn" type="button" @click="browseFor('cwd')">Browse...</button>
            <button v-if="draft.cwd" class="btn" type="button" @click="draft.cwd = ''">Clear</button>
          </div>
          <span class="muted small">Commands start here and a file name without a folder is looked for here.</span>
        </div>

        <div class="field">
          <span>Other folders she may use</span>
          <div v-for="(r, i) in draft.roots" :key="i" class="row">
            <input v-model="draft.roots[i]" type="text" class="grow mono" spellcheck="false" :aria-label="`Folder ${i + 1}`" />
            <button class="btn" type="button" @click="browseFor(i)">Browse...</button>
            <button class="btn" type="button" :aria-label="`Remove folder ${i + 1}`" @click="draft.roots.splice(i, 1)">Remove</button>
          </div>
          <div><button class="btn" type="button" @click="draft.roots.push('')">Add a folder</button></div>
          <span class="muted small">Nothing outside the working folder and these is ever reachable, and files that may hold secrets stay out of reach inside them.</span>
        </div>

        <div class="field">
          <span>What she may do here</span>
          <div class="tablewrap">
            <table class="t" aria-label="permissions">
              <tbody>
                <tr v-for="c in TOOL_CLASSES" :key="c.id">
                  <td><strong>{{ c.label }}</strong><div class="muted small">{{ c.what }}</div></td>
                  <td>
                    <select v-model="draft.policy[c.id]" :aria-label="`${c.label} in this environment`">
                      <option value="">Use Misty's setting ({{ base[c.id] || '...' }})</option>
                      <option value="auto">Allow without asking</option>
                      <option value="ask">Ask me each time</option>
                      <option value="deny">Never</option>
                    </select>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <p v-if="risky.length" class="error-box small" role="note">
            {{ risky.join(' and ') }} will run without asking you. Anything a model decides to do here happens at once on this machine.
          </p>
        </div>

        <div class="field">
          <span>Variables for commands</span>
          <div v-for="(v, i) in draft.vars" :key="i" class="row">
            <input v-model="v.k" type="text" class="mono varname" spellcheck="false" :aria-label="`Variable ${i + 1} name`" placeholder="NAME" />
            <input v-model="v.v" :type="shown.has(i) ? 'text' : 'password'" class="grow mono" autocomplete="off" :aria-label="`Variable ${i + 1} value`" placeholder="value" />
            <button class="btn" type="button" @click="shown.has(i) ? shown.delete(i) : shown.add(i)">{{ shown.has(i) ? 'Hide' : 'Show' }}</button>
            <button class="btn" type="button" :aria-label="`Remove variable ${i + 1}`" @click="removeVar(i)">Remove</button>
          </div>
          <div><button class="btn" type="button" @click="draft.vars.push({ k: '', v: '' })">Add a variable</button></div>
          <span class="muted small">
            Only commands she runs here get these (they otherwise get almost nothing). They are kept as plain text on the machine the hub runs on,
            and a command can print them, so a model could read them: do not put in anything she must not see.
          </span>
        </div>

        <div class="row actions">
          <button class="btn primary" type="button" :disabled="busy || !dirty || !draft.name.trim()" @click="save">{{ draft.id ? 'Save changes' : 'Create' }}</button>
          <button v-if="draft.id" class="btn" type="button" :disabled="busy" @click="duplicate">Duplicate</button>
          <button v-if="draft.id" class="btn" type="button" :disabled="busy" @click="useIt">Use in a new Misty chat</button>
          <span class="grow"></span>
          <span v-if="dirty" class="muted small" role="status">unsaved changes</span>
          <button v-if="draft.id" class="btn danger" type="button" :disabled="busy" @click="askDelete = true">Delete</button>
        </div>
      </section>
    </div>

    <FolderPicker :open="picking !== null" :start="pickStart" :title="picking === 'cwd' ? 'Working folder' : 'Another folder'" @pick="picked" @cancel="picking = null" />
    <ConfirmDialog :open="askDelete" title="Delete this environment?" action="Delete" @confirm="doDelete" @cancel="askDelete = false">
      Chats that use "{{ draft.name }}" keep their own working folder and go back to Misty's own settings. Nothing in the folders is touched.
    </ConfirmDialog>
    <ConfirmDialog :open="askRisk" title="Allow this without asking?" action="Save anyway" @confirm="doSave" @cancel="askRisk = false">
      {{ risky.join(' and ') }} will run at once, without waiting for your answer, whenever a model asks for it in a chat that uses this environment.
    </ConfirmDialog>
    <ConfirmDialog :open="!!discard" title="Discard your changes?" action="Discard" @confirm="runDiscard" @cancel="discard = null">
      This environment has changes that are not saved.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { useRouter } from 'vue-router'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import FolderPicker from '@/components/FolderPicker.vue'
import { TOOL_CLASSES, misty, type Environment, type ToolClass } from '@/misty'

const router = useRouter()
type PolicyChoice = '' | 'auto' | 'ask' | 'deny'
interface Draft {
  id: string
  name: string
  description: string
  cwd: string
  roots: string[]
  vars: { k: string; v: string }[]
  policy: Record<ToolClass, PolicyChoice>
}

const blank = (): Draft => ({ id: '', name: '', description: '', cwd: '', roots: [], vars: [], policy: { read: '', write: '', exec: '', net: '', hub: '' } })
const fromEnv = (e: Environment): Draft => ({
  id: e.id,
  name: e.name,
  description: e.description || '',
  cwd: e.cwd || '',
  roots: [...(e.roots || [])],
  vars: Object.entries(e.vars || {}).map(([k, v]) => ({ k, v })),
  policy: { ...blank().policy, ...(e.policy as Record<string, PolicyChoice>) },
})
/** What is sent to the hub for a draft; also what "has it changed" compares. */
function wire(d: Draft) {
  const policy: Record<string, string> = {}
  for (const c of TOOL_CLASSES) if (d.policy[c.id]) policy[c.id] = d.policy[c.id]
  const vars: Record<string, string> = {}
  for (const x of d.vars) if (x.k.trim()) vars[x.k.trim()] = x.v
  return {
    name: d.name.trim(),
    description: d.description.trim(),
    cwd: d.cwd.trim() || null,
    roots: d.roots.map((r) => r.trim()).filter(Boolean),
    vars,
    policy,
  }
}

const envs = ref<Environment[]>([])
const loaded = ref(false)
const draft = reactive<Draft>(blank())
let baseline = JSON.stringify(wire(blank()))
const base = ref<Record<string, string>>({})
const error = ref('')
const note = ref('')
const busy = ref(false)
const askDelete = ref(false)
const askRisk = ref(false)
const discard = ref<(() => void) | null>(null)
const picking = ref<'cwd' | number | null>(null)
const shown = reactive(new Set<number>())

const dirty = computed(() => JSON.stringify(wire(draft)) !== baseline)
/** The classes this draft turns to "without asking" that are the ones worth a second look. */
const risky = computed(() =>
  (['exec', 'net'] as ToolClass[]).filter((c) => draft.policy[c] === 'auto').map((c) => TOOL_CLASSES.find((x) => x.id === c)!.what),
)
const pickStart = computed(() => (picking.value === 'cwd' ? draft.cwd : typeof picking.value === 'number' ? draft.roots[picking.value] : '') || null)

function load(d: Draft) {
  Object.assign(draft, d)
  baseline = JSON.stringify(wire(d))
  shown.clear()
}
/** Runs `go`, after asking when the draft has changes that would be lost. */
function ask(go: () => void) {
  if (dirty.value) discard.value = go
  else go()
}
function runDiscard() {
  const go = discard.value
  discard.value = null
  go?.()
}
const startNew = () => load(blank())
const select = (e: Environment) => load(fromEnv(e))

async function refresh(keep = '') {
  try {
    envs.value = await misty.envs()
    const cur = envs.value.find((e) => e.id === (keep || draft.id))
    if (cur) load(fromEnv(cur))
  } catch (e: any) {
    error.value = e?.message || String(e)
  }
  loaded.value = true
}

function removeVar(i: number) {
  draft.vars.splice(i, 1)
  shown.clear()
}

function browseFor(what: 'cwd' | number) {
  picking.value = what
}
function picked(path: string) {
  if (picking.value === 'cwd') draft.cwd = path
  else if (typeof picking.value === 'number') draft.roots[picking.value] = path
  picking.value = null
}

function save() {
  // turning commands or the network on without a question deserves a second look
  const was = envs.value.find((e) => e.id === draft.id)?.policy || {}
  const newlyRisky = (['exec', 'net'] as ToolClass[]).some((c) => draft.policy[c] === 'auto' && was[c] !== 'auto')
  if (newlyRisky) askRisk.value = true
  else doSave()
}
async function doSave() {
  askRisk.value = false
  busy.value = true
  error.value = ''
  note.value = ''
  try {
    const w = wire(draft)
    const saved = await misty.saveEnv({ ...(draft.id ? { id: draft.id } : {}), ...w })
    note.value = `Saved "${saved.name}".`
    await refresh(saved.id)
  } catch (e: any) {
    error.value = e?.message || String(e)
  } finally {
    busy.value = false
  }
}

function duplicate() {
  const copy = { ...JSON.parse(JSON.stringify(draft)), id: '', name: `${draft.name} copy` } as Draft
  Object.assign(draft, copy)
  baseline = JSON.stringify(wire(blank())) // a copy is unsaved by definition
  note.value = ''
}

async function doDelete() {
  askDelete.value = false
  busy.value = true
  error.value = ''
  try {
    const name = draft.name
    await misty.deleteEnv(draft.id)
    load(blank())
    note.value = `Deleted "${name}".`
    await refresh()
  } catch (e: any) {
    error.value = e?.message || String(e)
  } finally {
    busy.value = false
  }
}

function useIt() {
  try {
    localStorage.setItem('mm_misty_env', draft.id)
    localStorage.removeItem('mm_misty_cwd')
  } catch (_) {
    /* a convenience only */
  }
  router.push('/misty')
}

onMounted(async () => {
  await refresh()
  try {
    base.value = (await misty.status()).policy || {}
  } catch (_) {
    /* the options still work without showing Misty's own setting */
  }
})
</script>

<style scoped>
.layout { display: grid; grid-template-columns: 260px minmax(0, 1fr); gap: 14px; align-items: start; }
.list ul { list-style: none; padding: 0; margin: 8px 0 0; }
.list li { border-radius: 6px; }
.list li.on { background: var(--panel-2); }
.link { width: 100%; text-align: left; background: none; border: 0; padding: 6px 8px; color: var(--text); display: flex; flex-direction: column; min-width: 0; }
.link .t, .link .mono { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.edit > * + * { margin-top: 14px; }
.field { display: flex; flex-direction: column; gap: 6px; }
.field .row { gap: 6px; }
.varname { width: 11em; flex: none; }
.actions { align-items: center; flex-wrap: wrap; }
@media (max-width: 760px) {
  .layout { grid-template-columns: 1fr; }
}
</style>

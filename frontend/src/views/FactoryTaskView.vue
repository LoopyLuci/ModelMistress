<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/factory" class="btn">Back</router-link>
      <h2 class="grow">{{ t?.goal || id }}</h2>
      <router-link :to="{ path: '/traces', query: { task: id } }" class="btn">Traces of this task</router-link>
    </div>
    <Unavailable v-if="gone" what="Factory" op="factory.task.get" />
    <p v-else-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="note" class="muted" role="status">{{ note }}</p>

    <template v-if="t">
      <div class="card">
        <div class="row">
          <span class="mono">{{ t.id }}</span>
          <span class="pill" :class="t.state">{{ stateLabel(t.state) }}</span>
          <span v-if="t.step" class="muted">step {{ t.step }}</span>
          <span class="meter" role="progressbar" :aria-valuenow="progress" aria-valuemin="0" aria-valuemax="100"><span :style="{ width: progress + '%' }"></span></span>
          <span class="small">{{ progress }}%</span>
          <span class="grow"></span>
          <a v-if="t.state === 'awaiting_approval'" href="#plan" class="btn primary">Review and approve the plan</a>
          <button class="btn" :disabled="busy || !canResume" @click="resume()">{{ t.state === 'budget' || t.state === 'interrupted' ? 'Resume' : 'Start / resume' }}</button>
          <button class="btn danger" :disabled="busy || ['done', 'cancelled'].includes(t.state)" @click="confirmCancel = true">Cancel task</button>
        </div>
        <p v-if="t.error" :class="['awaiting_approval', 'budget'].includes(t.state) ? 'muted' : 'error-box'" role="status">{{ t.error }}</p>

        <dl class="kv usage" aria-label="usage">
          <dt>tokens</dt><dd>{{ num(usage.tokens) }} <span class="muted small">({{ num(usage.prompt_tokens) }} prompt, {{ num(usage.completion_tokens) }} completion)</span></dd>
          <dt>model calls</dt><dd>{{ num(usage.calls) }}<span v-if="usage.retries" class="muted small"> ({{ usage.retries }} retries)</span></dd>
          <dt>time</dt><dd>{{ fmtDuration(usage.wall_ms) }}</dd>
        </dl>

        <ol class="steps" aria-label="pipeline steps">
          <li v-for="s in steps" :key="s.name" :class="s.state">
            <span class="dot" aria-hidden="true"></span>
            <strong>{{ s.name }}</strong>
            <span class="pill" :class="s.state">{{ s.state }}</span>
            <span v-if="s.error" class="small" style="color: var(--bad)">{{ s.error }}</span>
            <span v-if="s.started_ms" class="muted small">{{ fmtWhen(s.started_ms) }}</span>
            <span v-if="s.brief" class="muted small mono">{{ s.brief }}</span>
          </li>
        </ol>

        <details v-if="canResume" class="resume">
          <summary>Resume options</summary>
          <p class="muted small">
            Raise a budget or change a model first, and/or re-open a step: the steps from there on run again (records and
            chunks the earlier steps made stay).
          </p>
          <div class="formgrid">
            <label class="field">Re-open from step
              <select v-model="rs.from"><option value="">continue where it stopped</option><option v-for="x in FACTORY_STEPS" :key="x" :value="x">{{ x }}</option></select>
            </label>
            <label class="field">Wall time budget (s)
              <input v-model.number="rs.wall_s" type="number" min="1" :placeholder="String(spec.budget?.wall_s ?? '')" />
            </label>
            <label class="field">Max records (budget)
              <input v-model.number="rs.max_records" type="number" min="1" :placeholder="String(spec.budget?.max_records ?? '')" />
            </label>
            <label class="field">Max tokens (hard stop)
              <input v-model.number="rs.max_tokens" type="number" min="1" :placeholder="String(spec.size?.max_tokens ?? '')" />
            </label>
            <label class="field">Records (target)
              <input v-model.number="rs.records" type="number" min="1" :placeholder="String(spec.size?.records ?? '')" />
            </label>
            <label class="field">Author model
              <input v-model="rs.author" type="text" :placeholder="spec.author" />
            </label>
            <label class="field">Reviewer model
              <input v-model="rs.reviewer" type="text" :placeholder="spec.reviewer || ''" />
            </label>
          </div>
          <button class="btn primary" :disabled="busy" @click="resume(true)">Resume with these changes</button>
        </details>
      </div>

      <div id="plan" class="card">
        <div class="row">
          <h3 class="grow">Plan</h3>
          <span v-if="t.plan_rev" class="muted small">revision {{ t.plan_rev }}</span>
          <span v-if="t.plan != null" class="pill" :class="t.plan_approved ? 'approved' : 'pending'">{{ t.plan_approved ? 'approved' : 'not approved' }}</span>
        </div>
        <p v-if="t.plan == null" class="muted">The author model has not proposed a plan yet.</p>
        <template v-else>
          <p class="muted small">
            Edit the plan as JSON (outline, subtopics, questions), or apply a merge patch to it (a null removes a key).
            <template v-if="spec.plan_approval"> Nothing is built on the plan until it is approved; then resume.</template>
          </p>
          <label class="sr-only" for="plan-json">Plan JSON</label>
          <textarea id="plan-json" v-model="planText" rows="12" class="mono" spellcheck="false"></textarea>
          <label class="field">Merge patch instead (JSON object, applied to the saved plan)
            <textarea v-model="patchText" rows="2" class="mono" spellcheck="false" placeholder='{"questions": null}'></textarea>
          </label>
          <p v-if="planError" class="error-box" role="alert">{{ planError }}</p>
          <div class="row">
            <button class="btn" :disabled="busy || !(planDirty || patchText.trim())" @click="savePlan(false)">Save plan</button>
            <button class="btn primary" :disabled="busy" @click="savePlan(true)">{{ planDirty || patchText.trim() ? 'Save and approve' : 'Approve plan' }}</button>
          </div>
        </template>
      </div>

      <div class="split">
        <div class="card">
          <h3>Records produced</h3>
          <p>{{ recordCount }} usable record(s)</p>
          <ul class="plain">
            <li v-for="r in recordKms" :key="r.km">
              <router-link :to="`/data/${encodeURIComponent(r.km)}`" class="mono">{{ r.km }}</router-link>
            </li>
          </ul>
          <p v-if="!recordKms.length" class="muted">No data KM yet.</p>
          <router-link v-if="id" :to="{ path: '/factory/review', query: { task: id } }" class="btn">
            Review what awaits a decision<span v-if="t.review_open"> ({{ t.review_open }})</span>
          </router-link>
        </div>
        <div class="card">
          <h3>KMs built</h3>
          <ul class="plain">
            <li v-for="k in builtKms" :key="k"><router-link :to="`/knowledge/${encodeURIComponent(k.split('@')[0])}`" class="mono">{{ k }}</router-link></li>
          </ul>
          <p v-if="!builtKms.length" class="muted">None yet.</p>
          <p v-for="a in t.artifacts || []" :key="a.id + a.version + a.stage" class="muted small mono">{{ a.id }}@{{ a.version }} {{ a.stage }}</p>
        </div>
      </div>

      <div v-if="t.summary && Object.keys(t.summary).length" class="card">
        <h3>Summary</h3>
        <pre class="mono spec">{{ JSON.stringify(t.summary, null, 2) }}</pre>
      </div>

      <div class="card">
        <div class="row">
          <h3 class="grow">Records</h3>
          <select v-model="recStatus" aria-label="record status" @change="load">
            <option value="">any status</option>
            <option v-for="x in ['pending', 'approved', 'rejected']" :key="x" :value="x">{{ x }}</option>
          </select>
        </div>
        <div v-for="r in t.record_items || []" :key="r.id" class="rec">
          <div class="row small">
            <span class="mono">{{ r.id }}</span><span class="pill" :class="r.status">{{ r.status }}</span>
            <span v-if="r.score != null" class="muted">score {{ Number(r.score).toFixed(2) }}</span>
            <span v-if="r.reason" class="muted">{{ r.reason }}</span>
          </div>
          <div>{{ preview(userText(r)) }}</div>
          <div class="muted">{{ preview(r.output) }}</div>
        </div>
        <p v-if="!(t.record_items || []).length" class="muted">No records{{ recStatus ? ' with that status' : '' }} yet.</p>
        <p v-else class="muted small">The first {{ t.record_items.length }} records.</p>
      </div>

      <div class="card">
        <h3>Logs</h3>
        <div class="logs mono" role="log" aria-live="off" tabindex="0" aria-label="task log">
          <div v-for="(l, i) in logs" :key="i" :class="l.level"><span class="muted">{{ l.ts ? fmtWhen(l.ts) : '' }}</span> {{ l.step ? `[${l.step}] ` : '' }}{{ l.msg || l.message || l }}</div>
          <div v-if="!logs.length" class="muted">No log lines.</div>
        </div>
      </div>

      <details class="card">
        <summary>The task as submitted</summary>
        <pre class="mono spec">{{ JSON.stringify(t.spec ?? t, null, 2) }}</pre>
      </details>
    </template>

    <ConfirmDialog :open="confirmCancel" title="Cancel this task?" action="Cancel task" @confirm="doCancel" @cancel="confirmCancel = false">
      A running step stops at the next safe point. What was produced so far is kept, and the task can be resumed.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import Unavailable from '@/components/Unavailable.vue'
import { FACTORY_STEPS, factory, fmtDuration, resumable, stateLabel, stepProgress, unavailable } from '@/api'
import { fmtWhen } from '@/util'

const props = defineProps<{ id: string }>()
const t = ref<any>(null)
const gone = ref(false)
const error = ref('')
const note = ref('')
const busy = ref(false)
const confirmCancel = ref(false)
const planText = ref('')
const planOrig = ref('')
const patchText = ref('')
const planError = ref('')
const recStatus = ref('')
const rs = reactive({
  from: '', wall_s: undefined as number | undefined, max_records: undefined as number | undefined, max_tokens: undefined as number | undefined,
  records: undefined as number | undefined, author: '', reviewer: '',
})
let timer = 0

// factory.task.get: {id, goal, state, step, error, usage, summary, spec, steps: [{name, status, started_ms, error, result}],
// plan, plan_rev, plan_approved, logs: [{ts, level, step, msg}], records: {<data km id>: count}, record_count, record_items,
// kms: ["id@version"], artifacts, review_open, running}.
const spec = computed<any>(() => t.value?.spec || {})
const usage = computed<any>(() => t.value?.usage || {})
const num = (n?: number) => (n ?? 0).toLocaleString()
const steps = computed(() => {
  const given: any[] = t.value?.steps || []
  return FACTORY_STEPS.map((name) => {
    const g = given.find((x) => x.name === name)
    const state = g?.status || 'waiting'
    const r = g?.result
    const brief = r && typeof r === 'object' && state !== 'waiting' ? briefOf(r) : ''
    return { name, state, error: g?.error, started_ms: g?.started_ms, brief }
  })
})
function briefOf(r: Record<string, any>): string {
  if (r.skipped) return `skipped: ${r.skipped}`
  const parts = Object.entries(r).filter(([, v]) => typeof v === 'number' || typeof v === 'string').slice(0, 4)
  return parts.map(([k, v]) => `${k} ${String(v).slice(0, 40)}`).join(', ')
}
const progress = computed(() => stepProgress(t.value?.state, t.value?.step, t.value?.steps))
const canResume = computed(() => !!t.value && resumable(t.value.state) && !t.value.running)
const logs = computed<any[]>(() => t.value?.logs || [])
const recordKms = computed(() => Object.keys(t.value?.records || {}).map((km) => ({ km })))
const recordCount = computed(() => t.value?.record_count ?? 0)
const builtKms = computed<string[]>(() => (t.value?.kms || []).map((k: any) => (typeof k === 'string' ? k : k.key || k.id)))
const planDirty = computed(() => planText.value !== planOrig.value)
const userText = (r: any) => [...(r.input || [])].reverse().find((m: any) => m.role === 'user')?.content || r.input?.[0]?.content || ''
const preview = (s?: string) => (s && s.length > 160 ? s.slice(0, 160) + '...' : s || '')

async function load() {
  window.clearTimeout(timer)
  try {
    const keepPlan = planDirty.value
    t.value = await factory.get(props.id, { records: 20, logs: 60, ...(recStatus.value ? { status: recStatus.value } : {}) })
    error.value = ''
    if (!keepPlan) {
      planOrig.value = planText.value = t.value.plan == null ? '' : JSON.stringify(t.value.plan, null, 2)
    }
  } catch (e: any) {
    if (unavailable(e)) gone.value = true
    else error.value = e.message
  }
  if (!gone.value) timer = window.setTimeout(load, 3000)
}

async function resume(withChanges = false) {
  busy.value = true
  error.value = ''
  try {
    const changes: Record<string, any> = {}
    if (withChanges) {
      const budget: Record<string, number> = {}
      if (rs.wall_s) budget.wall_s = rs.wall_s
      if (rs.max_records) budget.max_records = rs.max_records
      if (Object.keys(budget).length) changes.budget = budget
      const size: Record<string, number> = {}
      if (rs.max_tokens) size.max_tokens = rs.max_tokens
      if (rs.records) size.records = rs.records
      if (Object.keys(size).length) changes.size = size
      if (rs.author.trim()) changes.author = rs.author.trim()
      if (rs.reviewer.trim()) changes.reviewer = rs.reviewer.trim()
    }
    const r = await factory.resume(props.id, { ...(rs.from ? { from: rs.from } : {}), ...(Object.keys(changes).length ? { changes } : {}) })
    note.value = `Resumed${r?.resumed_from ? ` at ${r.resumed_from}` : ''}${r?.job ? ` as job ${r.job}` : ''}.`
    await load()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
async function doCancel() {
  confirmCancel.value = false
  busy.value = true
  error.value = ''
  try {
    await factory.cancel(props.id)
    note.value = 'Cancelled.'
    await load()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}

async function savePlan(approve: boolean) {
  planError.value = ''
  const a: { plan?: unknown; changes?: Record<string, unknown>; approve?: boolean } = {}
  try {
    if (planDirty.value) a.plan = JSON.parse(planText.value)
    if (patchText.value.trim()) a.changes = JSON.parse(patchText.value)
  } catch (e: any) {
    planError.value = `Not valid JSON: ${e.message}`
    return
  }
  if (approve) a.approve = true
  busy.value = true
  try {
    const r = await factory.planEdit(props.id, a)
    note.value = `${approve ? 'Plan approved' : 'Plan saved'} (revision ${r?.rev}).${approve ? ' Resume to continue.' : ''}`
    patchText.value = ''
    planOrig.value = planText.value
    await load()
    planOrig.value = planText.value = JSON.stringify(t.value.plan, null, 2)
  } catch (e: any) {
    planError.value = e.message
  } finally {
    busy.value = false
  }
}

onMounted(load)
onBeforeUnmount(() => window.clearTimeout(timer))
</script>

<style scoped>
.steps { list-style: none; margin-top: 12px; display: grid; gap: 6px; }
.steps li { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.dot { width: 10px; height: 10px; border-radius: 50%; background: var(--border); flex: none; }
.steps .done .dot { background: var(--good); }
.steps .skipped .dot { background: var(--muted); }
.steps .running .dot { background: var(--warn); }
.steps .failed .dot, .steps .interrupted .dot { background: var(--bad); }
.usage { margin-top: 10px; }
.resume { margin-top: 12px; }
.resume summary { cursor: pointer; color: var(--muted); }
.plain { list-style: none; margin-bottom: 8px; }
.rec { padding: 6px 0; border-bottom: 1px solid var(--border); font-size: 0.85rem; overflow-wrap: anywhere; }
.logs { max-height: 280px; overflow: auto; background: var(--panel-2); border-radius: 8px; padding: 8px; font-size: 0.8rem; white-space: pre-wrap; }
.logs .error { color: var(--bad); }
.logs .warn { color: var(--warn); }
.spec { overflow-x: auto; }
.meter { display: inline-block; width: 90px; height: 8px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 4px; vertical-align: middle; overflow: hidden; }
.meter > span { display: block; height: 100%; background: var(--accent); }
</style>

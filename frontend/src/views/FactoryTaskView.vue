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
          <span class="pill" :class="t.status">{{ t.status }}</span>
          <span v-if="t.step" class="muted">step {{ t.step }}</span>
          <span class="grow"></span>
          <button class="btn" :disabled="busy" @click="go('resume')">Resume</button>
          <button class="btn danger" :disabled="busy" @click="confirmCancel = true">Cancel task</button>
        </div>
        <p v-if="t.error" class="error-box">{{ t.error }}</p>
        <ol class="steps" aria-label="pipeline steps">
          <li v-for="s in steps" :key="s.name" :class="s.state">
            <span class="dot" aria-hidden="true"></span>
            <strong>{{ s.name }}</strong>
            <span class="pill" :class="s.state">{{ s.state }}</span>
            <span v-if="s.progress != null" class="muted small">{{ Math.round(s.progress > 1 ? s.progress : s.progress * 100) }}%</span>
            <span v-if="s.error" class="small" style="color: var(--bad)">{{ s.error }}</span>
            <span v-if="s.started_ms" class="muted small">{{ fmtWhen(s.started_ms) }}</span>
          </li>
        </ol>
      </div>

      <div class="card">
        <h3>Plan</h3>
        <p v-if="t.plan == null" class="muted">The author model has not proposed a plan yet.</p>
        <template v-else>
          <p class="muted small">Edit the plan as JSON (outline, subtopics, questions). Nothing is generated until the plan is approved.</p>
          <label class="sr-only" for="plan">Plan JSON</label>
          <textarea id="plan" v-model="planText" rows="12" class="mono" spellcheck="false"></textarea>
          <p v-if="planError" class="error-box" role="alert">{{ planError }}</p>
          <div class="row">
            <button class="btn" :disabled="busy || !planDirty" @click="savePlan(false)">Save plan</button>
            <button class="btn primary" :disabled="busy" @click="savePlan(true)">{{ planDirty ? 'Save and approve' : 'Approve plan' }}</button>
            <span v-if="t.plan_approved" class="pill approved">approved</span>
          </div>
        </template>
      </div>

      <div class="split">
        <div class="card">
          <h3>Records produced</h3>
          <p>{{ recordCount }} record(s)</p>
          <ul class="plain">
            <li v-for="r in recordKms" :key="r.km">
              <router-link :to="`/data/${encodeURIComponent(r.km)}`" class="mono">{{ r.km }}</router-link>
              <span v-if="r.count != null" class="muted small">{{ " " }}{{ r.count }} records</span>
            </li>
          </ul>
          <p v-if="!recordKms.length" class="muted">None yet.</p>
          <router-link v-if="id" :to="{ path: '/factory/review', query: { task: id } }" class="btn">Review what awaits a decision</router-link>
        </div>
        <div class="card">
          <h3>KMs built</h3>
          <ul class="plain">
            <li v-for="k in builtKms" :key="k"><router-link :to="`/knowledge/${encodeURIComponent(k.split('@')[0])}`" class="mono">{{ k }}</router-link></li>
          </ul>
          <p v-if="!builtKms.length" class="muted">None yet.</p>
        </div>
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
        <pre class="mono spec">{{ JSON.stringify(t.spec ?? t.request ?? t, null, 2) }}</pre>
      </details>
    </template>

    <ConfirmDialog :open="confirmCancel" title="Cancel this task?" action="Cancel task" @confirm="doCancel" @cancel="confirmCancel = false">
      The running step stops. What was produced so far is kept, and the task can be resumed.
    </ConfirmDialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import Unavailable from '@/components/Unavailable.vue'
import { FACTORY_STEPS, factory, unavailable } from '@/api'
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
const planError = ref('')
let timer = 0

// factory.task.get: {id, goal, status, step, steps?: [{name, status, progress, started_ms, error}], plan, plan_approved,
// logs?: [{ts, level, step, msg}], records?: {<data km id>: count} | [ids], kms?: ["id@version"], spec}. All optional.
const steps = computed(() => {
  const given: any[] = t.value?.steps || []
  const cur = FACTORY_STEPS.indexOf(String(t.value?.step || '').toLowerCase())
  return (given.length ? given.map((g) => g.name) : FACTORY_STEPS).map((name: string, i: number) => {
    const g = given.find((x) => x.name === name) || {}
    const state = g.status || g.state || (cur < 0 ? 'waiting' : i < cur ? 'done' : i === cur ? (t.value.status === 'failed' ? 'failed' : 'running') : 'waiting')
    return { name, state, progress: g.progress, error: g.error, started_ms: g.started_ms }
  })
})
const logs = computed<any[]>(() => t.value?.logs || [])
const recordKms = computed(() => {
  const r = t.value?.records
  if (!r) return t.value?.data_kms ? (t.value.data_kms as string[]).map((km) => ({ km, count: null })) : []
  if (Array.isArray(r)) return [...new Set(r.map((x: any) => x.km || x.kms || ''))].filter(Boolean).map((km) => ({ km: String(km), count: r.filter((x: any) => x.km === km).length }))
  return Object.entries(r).map(([km, count]) => ({ km, count: typeof count === 'number' ? count : null }))
})
const recordCount = computed(() => {
  const r = t.value?.records
  return typeof t.value?.record_count === 'number' ? t.value.record_count : Array.isArray(r) ? r.length : r ? Object.values(r).reduce((a: number, b: any) => a + (Number(b) || 0), 0) : 0
})
const builtKms = computed<string[]>(() => (t.value?.kms || t.value?.built || []).map((k: any) => (typeof k === 'string' ? k : k.key || k.id)))
const planDirty = computed(() => planText.value !== planOrig.value)

async function load() {
  try {
    const keepPlan = planDirty.value
    t.value = await factory.get(props.id)
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

async function go(what: 'resume' | 'cancel') {
  busy.value = true
  error.value = ''
  try {
    await factory[what](props.id)
    note.value = what === 'resume' ? 'Resumed.' : 'Cancelled.'
    window.clearTimeout(timer)
    await load()
  } catch (e: any) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
const doCancel = () => {
  confirmCancel.value = false
  go('cancel')
}

async function savePlan(approve: boolean) {
  planError.value = ''
  let plan: unknown
  try {
    plan = JSON.parse(planText.value)
  } catch (e: any) {
    planError.value = `The plan is not valid JSON: ${e.message}`
    return
  }
  busy.value = true
  try {
    await factory.planEdit(props.id, { ...(planDirty.value ? { plan } : {}), ...(approve ? { approve: true } : {}) })
    note.value = approve ? 'Plan approved.' : 'Plan saved.'
    planOrig.value = planText.value
    window.clearTimeout(timer)
    await load()
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
.steps .running .dot { background: var(--warn); }
.steps .failed .dot { background: var(--bad); }
.plain { list-style: none; margin-bottom: 8px; }
.logs { max-height: 280px; overflow: auto; background: var(--panel-2); border-radius: 8px; padding: 8px; font-size: 0.8rem; white-space: pre-wrap; }
.logs .error { color: var(--bad); }
.logs .warn { color: var(--warn); }
.spec { overflow-x: auto; }
</style>

<template>
  <div v-if="trace">
    <div class="card head">
      <div class="row">
        <strong>{{ trace.model || 'unknown model' }}</strong>
        <span class="pill" :class="trace.status">{{ trace.status }}</span>
        <span class="pill">{{ trace.kind }}</span>
        <span v-if="trace.task_id" class="pill" title="factory task">task {{ trace.task_id }}</span>
        <span v-if="trace.job_id" class="pill" title="job">job {{ trace.job_id }}</span>
        <span class="grow"></span>
        <label class="row muted"><input v-model="colour" type="checkbox" /> colour by probability</label>
      </div>
      <dl class="kv facts">
        <dt>id</dt><dd class="mono">{{ trace.id }}</dd>
        <dt>started</dt><dd>{{ when(trace.started_ms) }}</dd>
        <dt>tokens</dt><dd>{{ tokens.length }}<span v-if="tps"> &middot; {{ tps }} tokens/s</span></dd>
        <dt v-if="timings.prompt_n">prompt</dt>
        <dd v-if="timings.prompt_n">{{ timings.prompt_n }} tokens in {{ ms(timings.prompt_ms) }}</dd>
        <dt v-if="adapters.length">adapters</dt>
        <dd v-if="adapters.length">
          <span v-for="a in adapters" :key="a.index" class="pill mono">{{ a.name }} x{{ a.effective_scale }}</span>
        </dd>
        <dt v-if="trace.parent_id">rerun of</dt>
        <dd v-if="trace.parent_id"><router-link :to="`/traces/${trace.parent_id}`" class="mono">{{ trace.parent_id }}</router-link></dd>
      </dl>
      <p v-if="trace.error" class="error-box">{{ trace.error }}</p>
      <div class="legend muted">
        <span>sure</span><span class="ramp"></span><span>unsure</span>
        <span class="hint">Hover or tap a token for the alternatives the model weighed.</span>
      </div>
    </div>

    <details class="card" :open="false">
      <summary>Prompt</summary>
      <pre class="prompt">{{ trace.rendered_prompt || messagesText }}</pre>
      <p v-if="trace.rendered_prompt" class="muted">After the model's chat template.</p>
    </details>

    <details class="card" :open="thinkOpen" @toggle="thinkOpen = ($event.target as HTMLDetailsElement).open">
      <summary>Reasoning <span class="muted">({{ reasoningTokens.length }} tokens)</span></summary>
      <TokenStream
        v-if="reasoningTokens.length"
        :tokens="reasoningTokens"
        :colour="colour"
      />
      <div v-else-if="trace.reasoning" class="plain">{{ trace.reasoning }}</div>
      <p v-else class="muted">The model did not show any reasoning.</p>
    </details>

    <div class="card">
      <h3>Answer</h3>
      <TokenStream
        v-if="answerTokens.length"
        :tokens="answerTokens"
        :colour="colour"
      />
      <div v-else-if="trace.answer" class="plain">{{ trace.answer }}</div>
      <p v-else class="muted">{{ live ? 'Waiting for the first token...' : 'No answer recorded.' }}</p>
    </div>

    <div class="grid">
      <div class="card">
        <h3>Stages</h3>
        <Timeline :stages="trace.stages || []" :start-ms="trace.started_ms" :end-ms="trace.ended_ms" :live="live" :now="now" />
      </div>
      <div class="card">
        <h3>Resources</h3>
        <p v-if="!res" class="muted">Not recorded yet.</p>
        <table v-else class="res">
          <thead><tr><th></th><th>start</th><th>end</th></tr></thead>
          <tbody>
            <tr><td>hub RAM</td><td>{{ mb(res.start?.hub?.rss_mb) }}</td><td>{{ mb(res.end?.hub?.rss_mb) }}</td></tr>
            <tr v-for="m in modelRows" :key="m.model">
              <td>{{ m.model }}</td><td>{{ mb(m.a) }}</td><td>{{ mb(m.b) }}</td>
            </tr>
            <tr>
              <td>system free</td>
              <td>{{ mb(res.start?.system?.available_mb) }}</td><td>{{ mb(res.end?.system?.available_mb) }}</td>
            </tr>
          </tbody>
        </table>
        <p class="muted small">GPU memory is shown when the engine reports it; llama-server does not.</p>
      </div>
    </div>

    <div class="grid">
      <div class="card">
        <h3>Selected knowledge modules</h3>
        <p v-if="!selections.length" class="muted">
          None recorded. The KM runtime adds <span class="mono">selection</span> events (candidates, score terms, the final set).
        </p>
        <div v-for="(s, i) in selections" :key="i" class="sel">
          <div v-for="k in s.selected || []" :key="k.id" class="row">
            <span class="mono">{{ k.id }}</span><span class="muted">{{ k.why || k.score }}</span>
          </div>
        </div>
      </div>
      <div class="card">
        <h3>Expert routing</h3>
        <RoutingHeatmap :events="trace.events || []" />
      </div>
    </div>

    <details v-if="(trace.specialist_calls || []).length" class="card">
      <summary>Specialist calls ({{ trace.specialist_calls.length }})</summary>
      <div v-for="c in trace.specialist_calls" :key="c.id" class="call">
        <span class="mono">{{ c.specialist }}</span> <span class="muted">{{ c.ms }} ms</span>
        <span class="pill" :class="c.ok ? 'ok' : 'error'">{{ c.ok ? 'ok' : 'failed' }}</span>
      </div>
    </details>

    <details v-if="(trace.events || []).length" class="card">
      <summary>Events ({{ trace.events.length }})</summary>
      <div v-for="e in trace.events" :key="e.id" class="ev">
        <span class="mono">{{ e.kind }}</span>
        <span class="mono muted">{{ JSON.stringify(e.data).slice(0, 300) }}</span>
      </div>
    </details>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import RoutingHeatmap from './RoutingHeatmap.vue'
import Timeline from './Timeline.vue'
import TokenStream from './TokenStream.vue'

const props = defineProps<{ trace: any; tokens: any[]; live?: boolean; now?: number }>()

const colour = ref(true)
const thinkOpen = ref(true)

const timings = computed(() => props.trace?.timings || {})
const adapters = computed(() => props.trace?.adapters || [])
const res = computed(() => props.trace?.resources && Object.keys(props.trace.resources).length ? props.trace.resources : null)
const reasoningTokens = computed(() => props.tokens.filter((t) => t.phase === 'reasoning'))
const answerTokens = computed(() => props.tokens.filter((t) => t.phase !== 'reasoning'))
const selections = computed(() => (props.trace?.events || []).filter((e: any) => e.kind === 'selection').map((e: any) => e.data))
const tps = computed(() => {
  const v = timings.value.predicted_per_second
  return v ? Number(v).toFixed(1) : ''
})
const messagesText = computed(() =>
  (props.trace?.request?.messages || []).map((m: any) => `${m.role}: ${typeof m.content === 'string' ? m.content : JSON.stringify(m.content)}`).join('\n\n'),
)
const modelRows = computed(() => {
  const a = res.value?.start?.models || []
  const b = res.value?.end?.models || []
  const names = [...new Set([...a, ...b].map((m: any) => m.model))]
  return names.map((n) => ({
    model: n,
    a: a.find((m: any) => m.model === n)?.rss_mb,
    b: b.find((m: any) => m.model === n)?.rss_mb,
  }))
})

const when = (ms: number) => new Date(ms).toLocaleString()
const ms = (v: number) => (v == null ? '' : v >= 1000 ? (v / 1000).toFixed(2) + ' s' : Math.round(v) + ' ms')
const mb = (v: number | null | undefined) => (v == null ? '-' : v >= 1024 ? (v / 1024).toFixed(1) + ' GB' : Math.round(v) + ' MB')
</script>

<style scoped>
.facts { margin-top: 10px; }
.legend { display: flex; align-items: center; gap: 8px; margin-top: 10px; font-size: 0.8rem; flex-wrap: wrap; }
.ramp { width: 90px; height: 8px; border-radius: 4px; background: linear-gradient(90deg, transparent, hsl(48 90% 50% / 0.35), hsl(0 90% 50% / 0.6)); border: 1px solid var(--border); }
.hint { margin-left: auto; }
.prompt { white-space: pre-wrap; word-break: break-word; font-family: var(--mono); font-size: 0.85rem; max-height: 320px; overflow: auto; }
.plain { white-space: pre-wrap; word-break: break-word; }
.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 14px; }
.res { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
.res th, .res td { text-align: left; padding: 2px 6px; }
.res td:nth-child(n + 2), .res th:nth-child(n + 2) { text-align: right; }
.small { font-size: 0.75rem; margin-top: 6px; }
.call, .ev, .sel .row { margin: 4px 0; display: flex; gap: 8px; flex-wrap: wrap; }
.ev span:last-child { word-break: break-all; }
.facts dd { min-width: 0; word-break: break-word; }
</style>

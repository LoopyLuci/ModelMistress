<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/traces" class="btn">Back</router-link>
      <h2 class="grow">Compare</h2>
    </div>
    <p v-if="error" class="error-box">{{ error }}</p>
    <p v-else-if="!d" class="muted">Comparing...</p>
    <template v-else>
      <div class="grid">
        <div v-for="(s, side) in { A: d.a, B: d.b }" :key="side" class="card">
          <h3>{{ side }}</h3>
          <router-link :to="`/traces/${s.id}`" class="mono">{{ s.id }}</router-link>
          <dl class="kv">
            <dt>model</dt><dd>{{ s.model }}</dd>
            <dt>tokens</dt><dd>{{ s.token_count }}</dd>
            <dt>speed</dt><dd>{{ s.tokens_per_s ? Number(s.tokens_per_s).toFixed(1) + ' tok/s' : '-' }}</dd>
            <dt>time</dt><dd>{{ s.wall_ms ? (s.wall_ms / 1000).toFixed(2) + ' s' : '-' }}</dd>
          </dl>
        </div>
      </div>

      <div class="card">
        <h3>What differs</h3>
        <p v-if="!d.same_model" class="warn">Different models.</p>
        <p v-if="!d.same_prompt" class="warn">Different prompts.</p>
        <div v-for="p in d.params" :key="p.key" class="row">
          <span class="mono">{{ p.key }}</span>
          <span class="mono del">{{ show(p.a) }}</span>
          <span class="muted">to</span>
          <span class="mono ins">{{ show(p.b) }}</span>
        </div>
        <div v-if="d.adapters.changed" class="row">
          <span class="muted">adapters</span>
          <span class="mono">{{ adapters(d.adapters.a) }}</span> <span class="muted">to</span>
          <span class="mono">{{ adapters(d.adapters.b) }}</span>
        </div>
        <p v-if="d.same_model && d.same_prompt && !d.params.length && !d.adapters.changed" class="muted">
          Same model, prompt, settings and adapters.
        </p>
      </div>

      <div class="card">
        <h3>Answer <span class="muted">({{ (d.answer.similarity * 100).toFixed(0) }}% of words shared)</span></h3>
        <p v-if="d.answer.equal" class="muted">Identical.</p>
        <div v-else class="text"><span v-for="(op, i) in d.answer.ops" :key="i" :class="op.op">{{ op.text }}</span></div>
      </div>

      <details v-if="!d.reasoning.equal" class="card">
        <summary>Reasoning</summary>
        <div class="text"><span v-for="(op, i) in d.reasoning.ops" :key="i" :class="op.op">{{ op.text }}</span></div>
      </details>

      <div class="card">
        <h3>Where the tokens first differ</h3>
        <p v-if="!div" class="muted">They never do.</p>
        <template v-else>
          <p>
            At token {{ div.index }}, after <span class="mono">{{ div.context }}</span>
          </p>
          <div class="grid">
            <div v-for="(s, side) in { A: div.a, B: div.b }" :key="side">
              <strong>{{ side }}</strong>
              <div v-if="s">
                <span class="mono">{{ JSON.stringify(s.text) }}</span> <span class="muted">p = {{ (s.p * 100).toFixed(1) }}%</span>
                <div v-for="(a, i) in s.alts" :key="i" class="muted mono small">
                  {{ JSON.stringify(a.text) }} {{ (a.p * 100).toFixed(1) }}%
                </div>
              </div>
              <span v-else class="muted">(ended)</span>
            </div>
          </div>
        </template>
        <dl class="kv stats">
          <dt>mean log probability</dt>
          <dd>{{ d.tokens.a.mean_logprob.toFixed(3) }} vs {{ d.tokens.b.mean_logprob.toFixed(3) }}</dd>
          <dt>mean entropy</dt>
          <dd>{{ d.tokens.a.mean_entropy.toFixed(3) }} vs {{ d.tokens.b.mean_entropy.toFixed(3) }}</dd>
          <dt>uncertain tokens (p &lt; 50%)</dt>
          <dd>{{ d.tokens.a.uncertain }} vs {{ d.tokens.b.uncertain }}</dd>
        </dl>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { call } from '@/transport'

const props = defineProps<{ a: string; b: string }>()
const d = ref<any>(null)
const error = ref('')
const div = computed(() => d.value?.tokens.first_divergence)
const show = (v: any) => (v === null || v === undefined ? '(unset)' : typeof v === 'string' ? v : JSON.stringify(v))
const adapters = (l: any[]) => (l.length ? l.map((x) => `${x.name} x${x.scale}`).join(', ') : 'none')

onMounted(async () => {
  try {
    d.value = await call('trace.diff', { a: props.a, b: props.b })
  } catch (e: any) {
    error.value = e.message
  }
})
</script>

<style scoped>
.bar { margin-bottom: 10px; }
.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 14px; margin-bottom: 14px; }
.grid .card { margin-bottom: 0; }
.text { white-space: pre-wrap; word-break: break-word; line-height: 1.7; }
.del, span.del { background: var(--del); text-decoration: line-through; }
.ins, span.ins { background: var(--ins); }
.warn { color: var(--warn); }
.small { font-size: 0.8rem; }
.stats { margin-top: 12px; }
</style>

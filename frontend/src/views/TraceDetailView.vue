<template>
  <div class="page">
    <div class="row bar">
      <router-link to="/traces" class="btn">Back</router-link>
      <h2 class="grow">Trace</h2>
      <button class="btn" @click="rerunOpen = !rerunOpen">Rerun / ablate</button>
      <button class="btn danger" @click="remove">Delete</button>
    </div>
    <p v-if="error" class="error-box">{{ error }}</p>

    <div v-if="rerunOpen && trace" class="card">
      <h3>Run this request again</h3>
      <div class="row">
        <label class="row">temperature <input v-model="o.temperature" type="number" step="0.1" min="0" style="width: 80px" /></label>
        <label class="row">seed <input v-model="o.seed" type="number" style="width: 100px" /></label>
        <label class="row">max tokens <input v-model="o.max_tokens" type="number" min="1" style="width: 100px" /></label>
      </div>
      <div v-if="(trace.adapters || []).length" class="adapters">
        <div v-for="a in trace.adapters" :key="a.index" class="row">
          <span class="mono">{{ a.name }}</span>
          <span class="muted">was x{{ a.effective_scale }}</span>
          <label class="row"><input v-model="off" type="checkbox" :value="a.index" /> remove (scale 0)</label>
        </div>
      </div>
      <p class="muted small">Blank fields stay as they were. The new trace links back to this one and is compared with it.</p>
      <button class="btn primary" :disabled="running" @click="rerun">{{ running ? 'Running...' : 'Run' }}</button>
    </div>

    <TraceView v-if="trace" :trace="trace" :tokens="tokens" />
    <p v-else-if="!error" class="muted">Loading...</p>
    <button v-if="tokens.length < (trace?.token_count || 0)" class="btn" @click="more">Load more tokens</button>

    <div v-if="children.length" class="card">
      <h3>Reruns</h3>
      <div v-for="c in children" :key="c.id" class="row">
        <router-link :to="`/traces/${c.id}`" class="mono">{{ c.id }}</router-link>
        <span class="pill" :class="c.status">{{ c.status }}</span>
        <router-link :to="`/diff/${id}/${c.id}`">compare</router-link>
        <span class="muted ellipsis">{{ c.answer_preview }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import TraceView from '@/components/TraceView.vue'
import { call } from '@/transport'

const props = defineProps<{ id: string }>()
const router = useRouter()
const trace = ref<any>(null)
const tokens = ref<any[]>([])
const children = ref<any[]>([])
const error = ref('')
const rerunOpen = ref(false)
const running = ref(false)
const o = reactive<{ temperature: string | number; seed: string | number; max_tokens: string | number }>({
  temperature: '', seed: '', max_tokens: '',
})
const off = ref<number[]>([])

async function load() {
  error.value = ''
  trace.value = null
  tokens.value = []
  try {
    const [t, k, c] = await Promise.all([
      call('trace.get', { id: props.id }),
      call('trace.tokens', { id: props.id, limit: 5000 }),
      call('trace.list', { parent: props.id, limit: 20 }),
    ])
    trace.value = t
    tokens.value = k.tokens
    children.value = c.items
  } catch (e: any) {
    error.value = e.message
  }
}

async function more() {
  const k = await call('trace.tokens', { id: props.id, offset: tokens.value.length, limit: 5000 })
  tokens.value.push(...k.tokens)
}

async function rerun() {
  running.value = true
  error.value = ''
  const overrides: Record<string, number> = {}
  for (const k of ['temperature', 'seed', 'max_tokens'] as const) if (o[k] !== '') overrides[k] = Number(o[k])
  try {
    const r = await call('trace.rerun', {
      id: props.id,
      overrides,
      ...(off.value.length ? { remove_adapters: off.value } : {}),
    })
    router.push(`/diff/${props.id}/${r.trace}`)
  } catch (e: any) {
    error.value = e.message
  } finally {
    running.value = false
  }
}

async function remove() {
  if (!window.confirm('Delete this trace and its tokens?')) return
  try {
    await call('trace.delete', { id: props.id })
    router.push('/traces')
  } catch (e: any) {
    error.value = e.message
  }
}

onMounted(load)
watch(() => props.id, load)
</script>

<style scoped>
.bar { margin-bottom: 10px; }
.small { font-size: 0.8rem; margin: 8px 0; }
.adapters { margin-top: 8px; }
.ellipsis { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
</style>

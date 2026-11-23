<template>
  <div class="strip" role="status">
    <template v-if="st">
      <span class="muted small">The router chooses between</span>
      <span
        v-for="r in st.roles"
        :key="r.role"
        class="pill"
        :class="r.status === 'ready' ? 'ok' : 'warn'"
        :title="`${r.role}: ${r.model || 'no model'}${r.status !== 'ready' ? ' (' + (STATUS_TEXT[r.status] || r.status) + ')' : ''}${r.loaded ? ', loaded' : ''}`"
      >{{ r.role }}: {{ r.model ? short(r.model) : 'none' }}</span>
      <router-link class="small" :to="{ path: '/models', query: { tab: 'router' } }">Choose models</router-link>
    </template>
    <span v-else-if="error" class="muted small">{{ error }}</span>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { routerStatus, STATUS_TEXT, type RouterStatus } from '@/chat/router'

// `refresh` changes after every reply: a model the router loaded shows as loaded.
const props = defineProps<{ refresh?: number }>()
const st = ref<RouterStatus | null>(null)
const error = ref('')

/** A long model id (`org/repo:file`) shortened to its end. */
const short = (id: string) => (id.length > 34 ? '...' + id.slice(-31) : id)

async function load() {
  try {
    st.value = await routerStatus()
    error.value = ''
  } catch (e: any) {
    error.value = `The router is not available: ${e?.message || e}`
  }
}
onMounted(load)
watch(() => props.refresh, load)
</script>

<style scoped>
.strip { display: flex; align-items: center; flex-wrap: wrap; gap: 6px 8px; padding: 6px 14px; border-bottom: 1px solid var(--border); background: var(--panel); }
</style>

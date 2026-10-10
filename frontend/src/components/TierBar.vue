<template>
  <div class="tier">
    <div class="row">
      <strong>{{ label }}</strong>
      <span class="muted small">{{ fmtBytes(used) }} of {{ budget > 0 ? fmtBytes(budget) : 'no limit' }}</span>
      <span class="grow"></span>
      <span v-if="budget > 0" class="small" :class="{ warn: pct > 90 }">{{ pct.toFixed(0) }}%</span>
    </div>
    <div
      class="bar" role="img"
      :aria-label="`${label}: ${fmtBytes(used)} of ${budget > 0 ? fmtBytes(budget) : 'no limit'}`"
    >
      <router-link
        v-for="s in segs" :key="s.key" :to="`/knowledge/${encodeURIComponent(s.id)}`" class="seg"
        :class="{ pinned: s.pinned, busy: s.busy }"
        :style="{ width: s.w + '%', background: s.color }"
        :title="`${s.key} ${fmtBytes(s.bytes)}${s.pinned ? ' (pinned)' : ''}${s.busy ? ' (in use)' : ''}`"
      ></router-link>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { fmtBytes } from '@/util'
import type { Resident } from '@/api'

// Resident KMs drawn as segments sized by bytes; the whole bar is the budget (or the used total with no budget).
const props = defineProps<{ label: string; used: number; budget: number; items: Resident[] }>()
const pct = computed(() => (props.budget > 0 ? (props.used / props.budget) * 100 : 0))
const hue = (s: string) => {
  let h = 0
  for (const c of s) h = (h * 31 + c.charCodeAt(0)) % 360
  return `hsl(${h} 55% 52%)`
}
const segs = computed(() => {
  const whole = Math.max(props.budget, props.used, 1)
  return props.items.map((r) => ({
    key: r.key, id: r.id, bytes: r.bytes, w: Math.max((r.bytes / whole) * 100, 0.8), pinned: r.pinned, busy: r.in_flight > 0,
    color: hue(r.id),
  }))
})
</script>

<style scoped>
.tier { margin-bottom: 14px; }
.bar { display: flex; height: 22px; background: var(--panel-2); border: 1px solid var(--border); border-radius: 6px; overflow: hidden; margin-top: 4px; }
.seg { display: block; height: 100%; min-width: 3px; border-right: 1px solid var(--panel); }
.seg.pinned { background-image: repeating-linear-gradient(45deg, rgba(255,255,255,.35) 0 4px, transparent 4px 8px); }
.seg.busy { outline: 2px solid var(--warn); outline-offset: -2px; }
.warn { color: var(--warn); font-weight: 600; }
</style>

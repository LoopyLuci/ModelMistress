<template>
  <div>
    <p v-if="!grid.layers.length" class="muted">
      No expert routing recorded for this trace. A model that reports it (events of kind
      <span class="mono">routing</span>) fills this in: one row per layer, one column per expert.
    </p>
    <div v-else class="scroll">
      <div v-for="l in grid.layers" :key="l.layer" class="lrow">
        <span class="mono lab">L{{ l.layer }}</span>
        <span
          v-for="(w, e) in l.weights"
          :key="e"
          class="cell"
          :style="{ opacity: w > 0 ? 0.15 + 0.85 * (w / grid.max) : 0.06 }"
          :title="`layer ${l.layer}, expert ${e}: ${w.toFixed(2)}`"
        ></span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

// An event of kind "routing" carries {token, layer, experts: [ids], weights: [gate weights], n_experts}.
const props = defineProps<{ events: any[]; token?: number | null }>()

const grid = computed(() => {
  const by = new Map<number, number[]>()
  let max = 0
  for (const ev of props.events) {
    if (ev.kind !== 'routing') continue
    const d = ev.data || {}
    if (props.token != null && d.token != null && d.token !== props.token) continue
    const n = d.n_experts || Math.max(0, ...(d.experts || [])) + 1
    const row = by.get(d.layer) || new Array(n).fill(0)
    ;(d.experts || []).forEach((x: number, k: number) => {
      row[x] = (row[x] || 0) + (d.weights?.[k] ?? 1)
      max = Math.max(max, row[x])
    })
    by.set(d.layer, row)
  }
  const layers = [...by.entries()].sort((a, b) => a[0] - b[0]).map(([layer, weights]) => ({ layer, weights }))
  return { layers, max: max || 1 }
})
</script>

<style scoped>
.scroll { overflow-x: auto; }
.lrow { display: flex; align-items: center; gap: 1px; margin-bottom: 1px; }
.lab { width: 34px; flex: none; color: var(--muted); }
.cell { width: 9px; height: 9px; background: var(--accent); flex: none; border-radius: 2px; }
</style>

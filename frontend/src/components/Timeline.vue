<template>
  <div class="tl">
    <div v-if="!rows.length" class="muted">No stages recorded.</div>
    <div v-for="r in rows" :key="r.key" class="line">
      <span class="name">{{ r.name }}</span>
      <span class="track"
        ><span class="bar" :class="r.name" :style="{ left: r.left + '%', width: r.width + '%' }"></span
      ></span>
      <span class="mono dur">{{ r.label }}</span>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{ stages: any[]; startMs: number; endMs?: number | null; live?: boolean; now?: number }>()

const fmt = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + ' s' : Math.round(ms) + ' ms')

const rows = computed(() => {
  const now = props.live ? (props.now ?? Date.now()) : 0
  const ends = props.stages.map((s) => s.ended_ms ?? s.end ?? now ?? 0)
  const last = Math.max(props.endMs ?? 0, now, ...ends, props.startMs + 1)
  const total = Math.max(1, last - props.startMs)
  return props.stages.map((s, i) => {
    const a = s.started_ms ?? s.start
    const b = s.ended_ms ?? s.end ?? (props.live ? now : a)
    return {
      key: i,
      name: s.name,
      left: Math.max(0, ((a - props.startMs) / total) * 100),
      width: Math.max(0.6, ((b - a) / total) * 100),
      label: fmt(b - a) + (s.ended_ms == null && props.live ? ' ...' : ''),
    }
  })
})
</script>

<style scoped>
.line { display: grid; grid-template-columns: 100px 1fr 80px; align-items: center; gap: 8px; margin: 3px 0; }
.name { font-size: 0.85rem; color: var(--muted); overflow: hidden; text-overflow: ellipsis; }
.track { position: relative; height: 12px; background: var(--panel-2); border-radius: 6px; overflow: hidden; }
.bar { position: absolute; top: 0; bottom: 0; background: var(--accent); border-radius: 6px; opacity: 0.75; }
.bar.prepare { background: var(--muted); }
.bar.prompt_eval { background: var(--warn); }
.bar.generation { background: var(--good); }
.dur { text-align: right; }
@media (max-width: 640px) {
  .line { grid-template-columns: 70px 1fr 64px; }
}
</style>

<template>
  <div class="stream">
    <span
      v-for="t in tokens"
      :key="t.i"
      class="tok"
      :class="{ pinned: pinned === t.i }"
      :style="colour ? tint(t) : undefined"
      @mouseenter="show($event, t)"
      @mouseleave="hide(t)"
      @click.stop="pin($event, t)"
      >{{ t.text }}</span
    >
    <span v-if="!tokens.length" class="muted">{{ empty }}</span>
    <Teleport to="body">
      <div v-if="tip" ref="box" class="tip" :style="{ left: pos.x + 'px', top: pos.y + 'px' }" @click.stop>
        <div class="head">
          <span class="mono q">{{ visible(tip.text) }}</span>
          <span class="muted">token {{ tip.i }}</span>
        </div>
        <div class="muted">
          p = {{ pct(tip.p) }} &middot; entropy {{ tip.entropy.toFixed(2) }} nats &middot; {{ tip.phase }}
        </div>
        <div v-for="(a, k) in tip.alts" :key="k" class="alt">
          <span class="bar" :style="{ width: Math.max(1, a.p * 100) + '%' }"></span>
          <span class="mono label" :class="{ chosen: a.text === tip.text }">{{ visible(a.text) }}</span>
          <span class="mono pct">{{ pct(a.p) }}</span>
        </div>
        <div v-if="!tip.alts.length" class="muted">no alternatives recorded</div>
      </div>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref } from 'vue'

defineProps<{ tokens: any[]; colour: boolean; empty?: string }>()

const tip = ref<any>(null)
const pinned = ref<number | null>(null)
const pos = ref({ x: 0, y: 0 })
const box = ref<HTMLElement | null>(null)

// Certain tokens stay plain; the less sure the model was, the stronger the tint, from amber towards red.
function tint(t: any) {
  const p = Math.min(1, Math.max(0, t.p))
  const hue = Math.round(48 * p)
  const alpha = (Math.pow(1 - p, 0.8) * 0.6).toFixed(2)
  return { background: `hsl(${hue} 90% 50% / ${alpha})` }
}

const pct = (p: number) => (p >= 0.995 ? '100%' : p < 0.001 ? '<0.1%' : (p * 100).toFixed(p < 0.1 ? 1 : 0) + '%')
const visible = (s: string) => JSON.stringify(s).slice(1, -1) || '(empty)'

async function place(ev: MouseEvent | { currentTarget: EventTarget | null }) {
  const el = ev.currentTarget as HTMLElement
  const r = el.getBoundingClientRect()
  pos.value = { x: r.left, y: r.bottom + 6 }
  await nextTick()
  const b = box.value?.getBoundingClientRect()
  if (b) {
    const x = Math.min(Math.max(8, r.left), window.innerWidth - b.width - 8)
    const y = r.bottom + 6 + b.height > window.innerHeight ? Math.max(8, r.top - b.height - 6) : r.bottom + 6
    pos.value = { x, y }
  }
}

function show(ev: MouseEvent, t: any) {
  if (pinned.value !== null) return
  tip.value = t
  place(ev)
}

function hide(_t: any) {
  if (pinned.value === null) tip.value = null
}

// A tap or click keeps the box open (touch screens have no hover); another tap or Escape closes it.
function pin(ev: MouseEvent, t: any) {
  if (pinned.value === t.i) {
    pinned.value = null
    tip.value = null
    return
  }
  pinned.value = t.i
  tip.value = t
  place(ev)
}

const close = () => {
  pinned.value = null
  tip.value = null
}
const key = (e: KeyboardEvent) => e.key === 'Escape' && close()
onMounted(() => {
  window.addEventListener('click', close)
  window.addEventListener('keydown', key)
})
onBeforeUnmount(() => {
  window.removeEventListener('click', close)
  window.removeEventListener('keydown', key)
})
</script>

<style scoped>
.stream { white-space: pre-wrap; word-break: break-word; line-height: 1.7; }
.tok { border-radius: 3px; cursor: default; }
.tok:hover, .tok.pinned { outline: 1px solid var(--accent); }
.tip {
  position: fixed; z-index: 50; width: min(320px, calc(100vw - 16px)); background: var(--panel); color: var(--text);
  border: 1px solid var(--border); border-radius: 10px; padding: 10px 12px; box-shadow: 0 8px 28px rgba(0, 0, 0, 0.25);
  font-size: 0.85rem;
}
.head { display: flex; justify-content: space-between; gap: 8px; margin-bottom: 2px; }
.q { font-weight: 700; word-break: break-all; }
.alt { position: relative; display: flex; justify-content: space-between; gap: 8px; padding: 1px 4px; margin-top: 3px; }
.alt .bar { position: absolute; left: 0; top: 0; bottom: 0; background: var(--ins); border-radius: 3px; z-index: 0; }
.alt .label, .alt .pct { position: relative; z-index: 1; }
.alt .label { word-break: break-all; }
.alt .chosen { font-weight: 700; }
</style>

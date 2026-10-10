<template>
  <dialog ref="dlg" class="confirm" :aria-labelledby="uid" @close="onClose" @cancel="emit('cancel')">
    <h3 :id="uid">{{ title }}</h3>
    <p class="muted"><slot /></p>
    <div class="row end">
      <button class="btn" type="button" @click="dlg?.close('cancel')">Cancel</button>
      <button class="btn primary danger-fill" type="button" @click="dlg?.close('ok')">{{ action }}</button>
    </div>
  </dialog>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'

// A native modal dialog: Escape cancels, focus stays inside while it is open and returns to the opener after.
const props = defineProps<{ open: boolean; title: string; action: string }>()
const emit = defineEmits<{ (e: 'confirm'): void; (e: 'cancel'): void }>()
const dlg = ref<HTMLDialogElement | null>(null)
const uid = `cd-${Math.random().toString(36).slice(2, 8)}`

watch(
  () => props.open,
  (o) => {
    if (!dlg.value) return
    if (o && !dlg.value.open) dlg.value.showModal()
    if (!o && dlg.value.open) dlg.value.close('cancel')
  },
  { flush: 'post' },
)

const onClose = () => {
  if (dlg.value?.returnValue === 'ok') emit('confirm')
  else emit('cancel')
  if (dlg.value) dlg.value.returnValue = ''
}
</script>

<style scoped>
.confirm {
  background: var(--panel); color: var(--text); border: 1px solid var(--border); border-radius: 10px; padding: 16px;
  max-width: min(440px, 92vw); margin: auto;
}
.confirm::backdrop { background: rgba(0, 0, 0, 0.45); }
.confirm h3 { margin-bottom: 8px; }
.confirm p { margin-bottom: 14px; }
.end { justify-content: flex-end; }
.danger-fill { background: var(--bad); border-color: var(--bad); color: #fff; }
</style>

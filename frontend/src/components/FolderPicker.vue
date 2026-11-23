<template>
  <dialog ref="dlg" class="picker" :aria-labelledby="uid" @close="answer(null)" @cancel="answer(null)">
    <h3 :id="uid">{{ title || 'Choose a folder' }}</h3>
    <p class="muted small">Folders on the machine the hub runs on. Pick one, or type or paste a full path.</p>

    <form class="row" @submit.prevent="go(typed)">
      <label class="sr-only" :for="`${uid}-path`">Folder path</label>
      <input :id="`${uid}-path`" v-model="typed" type="text" class="grow mono" spellcheck="false" placeholder="A full path, for example C:\Projects\app" />
      <button class="btn" type="submit" :disabled="busy">Go</button>
    </form>

    <div v-if="places.length" class="row places" role="group" aria-label="Places">
      <button v-for="p in places" :key="p.path" class="btn" type="button" :title="p.path" @click="go(p.path)">{{ p.label }}</button>
    </div>

    <p v-if="error" class="error-box small" role="alert">{{ error }}</p>

    <div class="folders" role="group" aria-label="Folders" :aria-busy="busy">
      <button v-if="listing?.parent" class="dir" type="button" @click="go(listing.parent!)">.. (up one folder)</button>
      <button v-for="d in listing?.dirs || []" :key="d.path" class="dir" type="button" :title="d.path" @click="go(d.path)">{{ d.name }}</button>
      <p v-if="listing?.path && !listing.dirs.length" class="muted small pad">No folders in here.</p>
      <p v-if="!listing?.path && !busy" class="muted small pad">Choose a place above, or type a path.</p>
      <p v-if="listing?.truncated" class="muted small pad">Only the first folders are listed: type the path to go further.</p>
    </div>
    <label class="row small"><input v-model="hidden" type="checkbox" /> show hidden folders (names starting with a dot)</label>

    <div class="row end">
      <span class="mono small grow cur" :title="listing?.path || ''">{{ listing?.path || 'no folder chosen yet' }}</span>
      <button class="btn" type="button" @click="finish(null)">Cancel</button>
      <button class="btn primary" type="button" :disabled="!listing?.path" @click="finish(listing?.path || null)">Use this folder</button>
    </div>
  </dialog>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'
import { misty, type Browse } from '@/misty'

// A native modal dialog like ConfirmDialog: Escape cancels, focus stays inside and returns to the opener after.
// It lists folders only (misty.browse), so it works the same in a browser and in the desktop app, and on a hub
// that runs on another machine, where the machine's own folder dialog would show the wrong disks.
const props = defineProps<{ open: boolean; start?: string | null; title?: string }>()
const emit = defineEmits<{ (e: 'pick', path: string): void; (e: 'cancel'): void }>()
const dlg = ref<HTMLDialogElement | null>(null)
const uid = `fp-${Math.random().toString(36).slice(2, 8)}`
const listing = ref<Browse | null>(null)
const places = ref<Browse['places']>([])
const typed = ref('')
const hidden = ref(false)
const busy = ref(false)
const error = ref('')

async function go(path: string) {
  const p = path.trim().replace(/^"|"$/g, '')
  busy.value = true
  error.value = ''
  try {
    const r = await misty.browse(p || undefined, hidden.value)
    if (r.places?.length) places.value = r.places
    listing.value = r
    typed.value = r.path || ''
  } catch (e: any) {
    // the old listing stays: a typo should not lose the place
    error.value = e?.message || String(e)
  } finally {
    busy.value = false
  }
}

watch(hidden, () => go(listing.value?.path || ''))

watch(
  () => props.open,
  async (o) => {
    if (!dlg.value) return
    if (o && !dlg.value.open) {
      answered = false
      listing.value = null
      error.value = ''
      typed.value = props.start || ''
      dlg.value.showModal()
      await go(props.start || '')
      // a folder that is gone: the places are still shown, with the reason
      if (!listing.value && props.start) await go('')
    }
    if (!o && dlg.value.open) dlg.value.close('cancel')
  },
  { flush: 'post' },
)

// The answer goes out from the button itself, not from the dialog's close event: that event only arrives once the page
// has drawn a frame, and a page that is not being drawn (a background tab, a hidden pane) would never answer.
// Escape and a close from outside still end in answer(null) through the dialog's own events; it answers once per opening.
let answered = true
function answer(path: string | null) {
  if (answered) return
  answered = true
  if (path) emit('pick', path)
  else emit('cancel')
}
function finish(path: string | null) {
  answer(path)
  dlg.value?.close(path ? 'ok' : 'cancel')
}
</script>

<style scoped>
.picker { background: var(--panel); color: var(--text); border: 1px solid var(--border); border-radius: 10px; padding: 16px; width: min(600px, 94vw); margin: auto; }
.picker::backdrop { background: rgba(0, 0, 0, 0.45); }
.picker > * + * { margin-top: 10px; }
.places { flex-wrap: wrap; gap: 6px; }
.folders { border: 1px solid var(--border); border-radius: 8px; height: min(320px, 40vh); overflow-y: auto; display: flex; flex-direction: column; background: var(--panel-2); }
.dir { text-align: left; background: none; border: 0; border-bottom: 1px solid var(--border); padding: 7px 10px; border-radius: 0; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: none; }
.dir:hover, .dir:focus-visible { background: var(--panel); }
.dir.up { color: var(--muted); }
.pad { padding: 8px 10px; }
.end { align-items: center; }
.cur { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>

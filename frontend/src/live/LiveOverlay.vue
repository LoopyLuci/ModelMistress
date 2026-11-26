<template>
  <section class="live card" aria-label="Live conversation">
    <div class="row head">
      <h3 class="grow">Live</h3>
      <span class="pill" :class="view.phase">{{ PHASE_TEXT[view.phase] }}</span>
      <button class="btn" type="button" :aria-pressed="view.micOn" @click="toggleMic">{{ view.micOn ? 'Mic on' : 'Mic off' }}</button>
      <label class="small check"><input type="checkbox" :checked="view.editBeforeSend" @change="toggleEdit" /> Edit before send</label>
      <button v-if="canPicture" class="btn" type="button" :aria-pressed="camOn" @click="toggleCamera">{{ camOn ? 'Camera off' : 'Camera on' }}</button>
      <button class="btn danger" type="button" @click="endAndClose">End</button>
    </div>

    <div v-if="camOn" class="cam">
      <video ref="videoEl" autoplay muted playsinline aria-label="Camera preview"></video>
      <p class="muted small">The picture goes with each thing you say, to a model that can read pictures.</p>
    </div>
    <p v-if="camError" class="error-box" role="alert">{{ camError }}</p>

    <p class="heard" aria-live="polite"><span class="muted small">You:</span> {{ view.heard || '...' }}</p>

    <div v-if="view.held !== null" class="held">
      <label class="small" for="live-held">
        {{ view.editBeforeSend ? 'Sentence to send: change it, then press Send now.' : 'Sending soon. Cancel send, or say "cancel", takes it back.' }}
      </label>
      <textarea id="live-held" :value="view.held" rows="2" aria-label="Sentence about to be sent" @input="editHeld"></textarea>
      <div class="row">
        <button class="btn primary" type="button" :disabled="!view.held.trim()" @click="sendHeld">Send now</button>
        <button class="btn" type="button" @click="cancelHeld">Cancel send</button>
      </div>
    </div>

    <p v-if="view.awaitingCorrection" class="muted small" role="status">Say the correction.</p>
    <p v-if="view.notice" class="muted small" role="status">{{ view.notice }}</p>
    <p v-if="view.error || problem" class="error-box" role="alert">{{ view.error || problem }}</p>

    <div class="row">
      <button class="btn" type="button" :disabled="view.phase !== 'speaking' && view.phase !== 'thinking'" @click="interrupt">Interrupt</button>
    </div>
    <p class="muted small">
      Talk as you would to a person. Start with "correction" or "that's wrong", then say what is right, to correct the last reply.
      Say "cancel" to take back a sentence.
    </p>
  </section>
</template>

<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { call } from '@/transport'
import { notInBuild, transcribeClip } from '@/speech'
import { captureFrame, startCamera, stopCamera } from '@/chat/camera'
import { LiveMic, LiveVoice } from './audio'
import { HOLD_MS, LiveTurns, type LiveEffect, type LiveInput, type LivePhase } from './turns'

const props = defineProps<{
  /** Sends the owner's words as the next message in this chat, with the pictures given. False when no model can answer. */
  send: (text: string, images: string[]) => boolean
  /** Stops the reply the chat is writing. */
  stop: () => void
  /** Whether the model in this chat can read pictures (only then can the camera be used). */
  canPicture: boolean
}>()
const emit = defineEmits<{ close: [] }>()

const PHASE_TEXT: Record<LivePhase, string> = {
  starting: 'Starting',
  listening: 'Listening',
  thinking: 'Thinking',
  speaking: 'Speaking',
  stopped: 'Stopped',
}

const turns = new LiveTurns()
/** What the screen shows: a copy of the turns' state, refreshed after each input. */
const view = reactive<{
  phase: LivePhase
  heard: string
  held: string | null
  error: string | null
  notice: string | null
  micOn: boolean
  editBeforeSend: boolean
  awaitingCorrection: boolean
}>({
  phase: 'starting',
  heard: '',
  held: null,
  error: null,
  notice: null,
  micOn: true,
  editBeforeSend: false,
  awaitingCorrection: false,
})
/** Problems outside the conversation itself: the microphone, speech, saving a correction, the camera. */
const problem = ref('')
const camOn = ref(false)
const camError = ref('')
const videoEl = ref<HTMLVideoElement | null>(null)
let camStream: MediaStream | null = null
let mic: LiveMic | null = null
let voice: LiveVoice | null = null
let holdTimer: number | undefined
/** Utterances are transcribed one after another, so that they are heard in the order they were said. */
let heardChain: Promise<void> = Promise.resolve()

function sync() {
  view.phase = turns.phase
  view.heard = turns.heard
  view.held = turns.held
  view.error = turns.error
  view.notice = turns.notice
  view.micOn = turns.micOn
  view.editBeforeSend = turns.editBeforeSend
  view.awaitingCorrection = turns.awaitingCorrection
}

function dispatch(input: LiveInput) {
  const effects = turns.handle(input)
  sync()
  for (const e of effects) run(e)
}

/** Carries out one effect of the conversation. Results come back as later inputs, never in this call. */
function run(e: LiveEffect) {
  switch (e.kind) {
    case 'listen':
      if (turns.micOn) mic?.setEnabled(true)
      break
    case 'cancelListening':
      mic?.setEnabled(false)
      break
    case 'hold':
      window.clearTimeout(holdTimer)
      if (e.timed) holdTimer = window.setTimeout(() => dispatch({ kind: 'holdEnded' }), HOLD_MS)
      break
    case 'send': {
      const started = props.send(e.text, pictureNow())
      queueMicrotask(() => dispatch({ kind: 'sent', started }))
      break
    }
    case 'stopReply':
      props.stop()
      break
    case 'say':
      voice?.say(e.sentence, (failure) => {
        if (failure) problem.value = `The reply could not be spoken: ${(failure as any)?.message || failure}`
        dispatch({ kind: 'spoken' })
      })
      break
    case 'stopSpeech':
      voice?.stop()
      break
    case 'record':
      void saveCorrection(e.question, e.answer, e.correction)
      break
  }
}

async function saveCorrection(question: string, answer: string, correction: string) {
  try {
    await call('misty.teach', { question, answer, correction })
  } catch (err: any) {
    problem.value = `The correction was not saved: ${err?.message || err}`
  }
}

/** One sentence the owner finished: written down by the hub's speech to text, then given to the conversation. */
async function heardClip(clip: Blob) {
  try {
    dispatch({ kind: 'said', text: await transcribeClip(clip) })
  } catch (err: any) {
    if (notInBuild(err)) {
      dispatch({ kind: 'failed', transient: false, message: 'Speech to text is not in this build of the hub.' })
      return
    }
    problem.value = `The sentence could not be written down: ${err?.message || err}`
    dispatch({ kind: 'failed', transient: true })
  }
}

function micMessage(err: any): string {
  if (err?.name === 'NotAllowedError') return 'The microphone is not allowed. Allow it for this page, then start Live again.'
  return `The microphone could not be opened: ${err?.message || err}`
}

function pictureNow(): string[] {
  if (!camOn.value || !props.canPicture || !videoEl.value) return []
  try {
    return [captureFrame(videoEl.value)]
  } catch (_) {
    return []
  }
}

async function toggleCamera() {
  if (camOn.value) {
    stopCamera(camStream)
    camStream = null
    camOn.value = false
    return
  }
  camError.value = ''
  try {
    camStream = await startCamera()
    camOn.value = true
    await nextTick()
    if (videoEl.value) {
      videoEl.value.srcObject = camStream
      await videoEl.value.play?.().catch(() => undefined)
    }
  } catch (err: any) {
    camError.value = `The camera is not available: ${err?.message || err}`
  }
}

function toggleMic() {
  dispatch({ kind: 'mic', on: !turns.micOn })
}
function toggleEdit(e: Event) {
  dispatch({ kind: 'editBeforeSend', on: (e.target as HTMLInputElement).checked })
}
function editHeld(e: Event) {
  dispatch({ kind: 'editHeld', text: (e.target as HTMLTextAreaElement).value })
}
function sendHeld() {
  window.clearTimeout(holdTimer)
  dispatch({ kind: 'sendHeld' })
}
function cancelHeld() {
  window.clearTimeout(holdTimer)
  dispatch({ kind: 'cancelHold' })
}
function interrupt() {
  dispatch({ kind: 'interrupt' })
}
function endAndClose() {
  dispatch({ kind: 'end' })
  emit('close')
}

/** The chat's reply as it stands: `busy` while it is written, and `error` when it failed. Called by the chat page. */
function reply(text: string | null, busy: boolean, error: string | null = null) {
  dispatch({ kind: 'reply', text, busy, error })
}
defineExpose({ reply })

onMounted(async () => {
  mic = new LiveMic({
    onUtterance: (clip) => {
      heardChain = heardChain.then(() => heardClip(clip))
    },
  })
  voice = new LiveVoice()
  dispatch({ kind: 'start' })
  try {
    await mic.open()
  } catch (err: any) {
    dispatch({ kind: 'failed', transient: false, message: micMessage(err) })
  }
})

onBeforeUnmount(() => {
  window.clearTimeout(holdTimer)
  // Ends the conversation: the reply stops, the speech stops, and the microphone goes quiet.
  dispatch({ kind: 'end' })
  mic?.close()
  voice?.stop()
  stopCamera(camStream)
  camStream = null
})
</script>

<style scoped>
.live { display: grid; gap: 10px; }
.head { flex-wrap: wrap; gap: 8px; }
.check { display: inline-flex; gap: 4px; align-items: center; }
.pill.listening { border-color: var(--good); }
.pill.speaking, .pill.thinking { border-color: var(--accent, var(--good)); }
.pill.stopped { border-color: var(--bad); }
.cam video { width: 100%; max-width: 360px; border-radius: 8px; background: #000; }
.heard { margin: 0; overflow-wrap: anywhere; }
.held textarea { width: 100%; margin: 4px 0 8px; }
</style>

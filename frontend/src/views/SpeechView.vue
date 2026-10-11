<template>
  <div class="page">
    <h2>Speech</h2>
    <section class="card">
      <h3>Speech to text</h3>
      <p v-if="sttOff" class="muted" role="status">Not available in this build: {{ sttOff }}</p>
      <template v-else>
        <div class="row">
          <button v-if="!recording" class="btn primary" type="button" @click="record">Record</button>
          <button v-else class="btn danger" type="button" @click="finish">Stop and transcribe ({{ secs }} s)</button>
          <label class="btn">Or choose an audio file<input type="file" accept="audio/*" hidden @change="onFile" /></label>
          <span v-if="sttBusy" class="muted">Transcribing...</span>
        </div>
        <p v-if="sttError" class="error-box" role="alert">{{ sttError }}</p>
        <textarea v-if="transcript" v-model="transcript" rows="5" aria-label="Transcript"></textarea>
        <div v-if="transcript" class="row">
          <button class="btn" type="button" @click="text = transcript">Use as text to speak</button>
          <router-link class="btn" to="/">Chat</router-link>
        </div>
      </template>
    </section>

    <section class="card">
      <h3>Text to speech</h3>
      <p v-if="ttsOff" class="muted" role="status">Not available in this build: {{ ttsOff }}</p>
      <template v-else>
        <textarea v-model="text" rows="4" placeholder="Text to speak" aria-label="Text to speak"></textarea>
        <div class="row">
          <button class="btn primary" type="button" :disabled="!text.trim() || ttsBusy" @click="speak">{{ ttsBusy ? 'Speaking...' : 'Speak' }}</button>
        </div>
        <p v-if="ttsError" class="error-box" role="alert">{{ ttsError }}</p>
        <audio v-if="audioUrl" :src="audioUrl" controls autoplay class="player"></audio>
      </template>
    </section>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'
import { failure, hubFetch } from '@/chat/hub'

const sttOff = ref('')
const ttsOff = ref('')
const sttError = ref('')
const ttsError = ref('')
const sttBusy = ref(false)
const ttsBusy = ref(false)
const recording = ref(false)
const secs = ref(0)
const transcript = ref('')
const text = ref('')
const audioUrl = ref('')
let rec: MediaRecorder | null = null
let chunks: Blob[] = []
let tick: number | undefined

const off = (e: any) => (e?.status === 501 || e?.status === 404 ? String(e.message) : '')

async function transcribe(blob: Blob, name: string) {
  sttBusy.value = true
  sttError.value = ''
  try {
    const fd = new FormData()
    fd.append('file', blob, name)
    const r = await hubFetch('/v1/audio/transcriptions', { method: 'POST', body: fd })
    if (!r.ok) throw await failure(r)
    transcript.value = ((await r.json()).text || '').trim()
    if (!transcript.value) sttError.value = 'No speech was recognised.'
  } catch (e: any) {
    sttOff.value = off(e)
    sttError.value = sttOff.value ? '' : String(e?.message || e)
  } finally {
    sttBusy.value = false
  }
}
async function record() {
  sttError.value = ''
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
    rec = new MediaRecorder(stream)
    chunks = []
    rec.ondataavailable = (e) => chunks.push(e.data)
    rec.onstop = () => {
      stream.getTracks().forEach((t) => t.stop())
      transcribe(new Blob(chunks, { type: rec?.mimeType || 'audio/webm' }), 'speech.webm')
    }
    rec.start()
    recording.value = true
    secs.value = 0
    tick = window.setInterval(() => secs.value++, 1000)
  } catch (e: any) {
    sttError.value = `The microphone is not available: ${e?.message || e}`
  }
}
function finish() {
  window.clearInterval(tick)
  recording.value = false
  rec?.stop()
}
function onFile(e: Event) {
  const el = e.target as HTMLInputElement
  const f = el.files?.[0]
  if (f) transcribe(f, f.name)
  el.value = ''
}
async function speak() {
  ttsBusy.value = true
  ttsError.value = ''
  try {
    const r = await hubFetch('/v1/audio/speech', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'tts-1', input: text.value.trim(), response_format: 'wav' }),
    })
    if (!r.ok) throw await failure(r)
    if (audioUrl.value) URL.revokeObjectURL(audioUrl.value)
    audioUrl.value = URL.createObjectURL(await r.blob())
  } catch (e: any) {
    ttsOff.value = off(e)
    ttsError.value = ttsOff.value ? '' : String(e?.message || e)
  } finally {
    ttsBusy.value = false
  }
}
onBeforeUnmount(() => {
  window.clearInterval(tick)
  if (recording.value) rec?.stream.getTracks().forEach((t) => t.stop())
  if (audioUrl.value) URL.revokeObjectURL(audioUrl.value)
})
</script>

<style scoped>
.card .row { margin: 8px 0; }
.player { width: 100%; margin-top: 8px; }
</style>

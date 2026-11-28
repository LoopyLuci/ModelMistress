<template>
  <section class="card" aria-label="Chat bots">
    <h3>Chat bots</h3>
    <p class="muted small">
      A bot answers the people you allow, with a model from this hub. Each bot has its own token, its own list of allowed
      people, its own model and its own instructions. Nothing is answered until someone is allowed. Save bots applies every
      change to every bot.
    </p>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="message" class="muted small" role="status">{{ message }}</p>

    <p v-if="!forms.length" class="muted small">No bots yet.</p>

    <article v-for="(f, i) in forms" :key="f.key" class="connect-tool" :aria-label="`Bot ${f.name.trim() || 'not named yet'}`">
      <div class="row">
        <h4 class="grow">{{ f.name.trim() || 'New bot' }}: {{ serviceLabel(f.kind) }}</h4>
        <label>
          <input v-model="f.enabled" type="checkbox" />
          On
        </label>
      </div>
      <p class="muted small" aria-live="polite">{{ statusOf(f) }}</p>

      <div class="row">
        <label class="grow">
          Name
          <input v-model="f.name" type="text" maxlength="64" />
        </label>
        <label>
          Service
          <select v-model="f.kind">
            <option v-for="s in SERVICES" :key="s.id" :value="s.id">{{ s.label }}</option>
          </select>
        </label>
      </div>

      <div class="row">
        <label class="grow">
          Token
          <input
            v-model="f.typedToken"
            :type="f.showToken ? 'text' : 'password'"
            autocomplete="off"
            spellcheck="false"
            :placeholder="f.hasSavedToken ? 'Saved: type a new one to replace it' : 'Paste the token here'"
            @input="f.clearToken = false"
          />
        </label>
        <button class="btn" type="button" :aria-pressed="f.showToken" @click="f.showToken = !f.showToken">
          {{ f.showToken ? 'Hide token' : 'Show token' }}
        </button>
        <button class="btn" type="button" :disabled="busy" @click="check(f)">Check token</button>
        <button v-if="f.hasSavedToken && !f.clearToken" class="btn" type="button" @click="f.clearToken = true">
          Remove saved token
        </button>
      </div>
      <p class="muted small">{{ tokenNote(f) }}</p>

      <div class="row">
        <label class="grow">
          {{ allowedLabel(f.kind) }}
          <input
            v-model="f.allowedText"
            type="text"
            :placeholder="f.kind === 'discord' ? 'e.g. 123456789012345678' : 'e.g. 123456789'"
          />
        </label>
      </div>
      <p class="muted small">{{ allowedHint(f.kind) }}</p>

      <div class="row">
        <label class="grow">
          Model
          <select v-model="f.model">
            <option v-for="o in modelOptionsFor(f)" :key="o.value" :value="o.value">{{ o.label }}</option>
          </select>
        </label>
        <label>
          Messages remembered
          <input v-model.number="f.remember" type="number" min="2" max="200" step="1" />
        </label>
      </div>
      <p v-if="modelsError" class="muted small">The model list did not load: {{ modelsError }}</p>

      <label>
        Instructions (optional)
        <textarea
          v-model="f.instructions"
          rows="3"
          maxlength="4000"
          placeholder="For example: answer in one short paragraph, in English."
        ></textarea>
      </label>

      <div class="row end">
        <button class="btn" type="button" :disabled="busy" @click="askRemove(i)">Remove this bot</button>
      </div>
    </article>

    <div class="row">
      <button class="btn" type="button" :disabled="busy" @click="add">Add a bot</button>
      <button class="btn primary" type="button" :disabled="busy" @click="save">Save bots</button>
    </div>

    <ConfirmDialog
      :open="pending !== null"
      title="Remove this bot?"
      action="Remove"
      @confirm="removeConfirmed"
      @cancel="pending = null"
    >
      {{ pendingName || 'This bot' }} stops answering, and its saved token is removed, when you save bots.
    </ConfirmDialog>
  </section>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import { listModels, isChatModel } from '@/chat/models'
import {
  allowedHint,
  allowedLabel,
  blankForm,
  checkRequest,
  formOf,
  modelOptions,
  SERVICES,
  serviceLabel,
  settingFromForm,
  statusLine,
  tokenNote,
  type BotForm,
  type BotSetting,
  type BotStatus,
} from '@/bots'
import { call } from '@/transport'

/** One bot in the form: the form's fields, plus what the panel keeps for it while it is shown. */
type Row = BotForm & { key: number; showToken: boolean }

const forms = ref<Row[]>([])
const statuses = ref<BotStatus[]>([])
const modelIds = ref<string[]>([])
const modelsError = ref('')
const busy = ref(false)
const error = ref('')
const message = ref('')
/** The index of the bot the owner is asked about removing; null when no question is open. */
const pending = ref<number | null>(null)
const pendingName = computed(() => (pending.value === null ? '' : forms.value[pending.value]?.name.trim() ?? ''))
let nextKey = 1
let timer: number | undefined

function rowOf(f: BotForm): Row {
  return { ...f, key: nextKey++, showToken: false }
}

async function load() {
  try {
    const cfg = await call('config.get', {})
    forms.value = ((cfg.bots || []) as BotSetting[]).map((b) => rowOf(formOf(b)))
    statuses.value = (await call('bots.status', {})) as BotStatus[]
  } catch (e: any) {
    error.value = `The bots did not load: ${e?.message || e}`
  }
}

async function loadModels() {
  try {
    modelIds.value = (await listModels()).filter(isChatModel).map((m) => m.id)
  } catch (e: any) {
    modelsError.value = String(e?.message || e)
  }
}

function statusOf(f: BotForm): string {
  return statusLine(f.savedName ? statuses.value.find((s) => s.name === f.savedName) : undefined)
}

function modelOptionsFor(f: BotForm) {
  return modelOptions(modelIds.value, f.model)
}

function add() {
  forms.value.push(rowOf(blankForm(forms.value)))
  message.value = 'A new bot. Name it, paste its token, allow someone, then save bots.'
}

function askRemove(i: number) {
  // a bot not saved yet has nothing to lose; a saved one is asked about first
  if (forms.value[i].savedName) pending.value = i
  else forms.value.splice(i, 1)
}

function removeConfirmed() {
  if (pending.value !== null) forms.value.splice(pending.value, 1)
  pending.value = null
  message.value = 'Removed in the form. Save bots to apply it.'
}

async function save() {
  error.value = ''
  message.value = ''
  busy.value = true
  try {
    const bots = forms.value.map((f) => settingFromForm(f))
    await call('config.set', { settings: { bots } })
    message.value = 'Saved. The bots that are on start now; the others stop.'
    await load()
  } catch (e: any) {
    error.value = `Not saved: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

async function check(f: Row) {
  error.value = ''
  message.value = ''
  const typed = f.typedToken.trim()
  if (f.clearToken && !typed) {
    error.value = 'The saved token is marked for removal. Type a token to check it.'
    return
  }
  if (!typed && !f.hasSavedToken) {
    error.value = 'Type the token first.'
    return
  }
  busy.value = true
  try {
    const r = await call('bots.check', checkRequest(f))
    message.value = `The token works: the bot is @${r.username}.${typed ? ' Save bots to keep this token.' : ''}`
  } catch (e: any) {
    error.value = `The token did not work: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  load()
  loadModels()
  timer = window.setInterval(async () => {
    try {
      statuses.value = (await call('bots.status', {})) as BotStatus[]
    } catch (_) {
      /* the hub is gone for a moment */
    }
  }, 5000)
})
onBeforeUnmount(() => window.clearInterval(timer))
</script>

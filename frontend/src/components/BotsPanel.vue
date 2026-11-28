<template>
  <section class="card" aria-label="Chat bots">
    <h3>Chat bots</h3>
    <p class="muted small">
      A Telegram or Discord bot answers the people you allow, with a model from this hub. For Telegram, create the bot with
      @BotFather and paste its token here. For Discord, create the bot in the developer portal, turn on its Message Content
      intent, invite it to a server, and paste its token. Someone who is not allowed is told their id, and you add it below.
      Nothing is answered until someone is allowed.
    </p>
    <p v-if="error" class="error-box" role="alert">{{ error }}</p>
    <p v-if="message" class="muted small" role="status">{{ message }}</p>

    <p v-if="!bots.length" class="muted small">No bots yet.</p>
    <div v-for="(b, i) in bots" :key="i" class="connect-tool">
      <div class="row">
        <label class="grow">
          Name
          <input v-model="b.name" type="text" maxlength="64" />
        </label>
        <label>
          Service
          <select v-model="b.kind">
            <option v-for="s in SERVICES" :key="s.id" :value="s.id">{{ s.label }}</option>
          </select>
        </label>
        <label>
          <input v-model="b.enabled" type="checkbox" />
          On
        </label>
      </div>
      <div class="row">
        <label class="grow">
          Token
          <input v-model="b.token" type="password" autocomplete="off" :placeholder="tokenHint(b)" />
        </label>
        <button class="btn" type="button" :disabled="busy" @click="check(b)">Check token</button>
      </div>
      <div class="row">
        <label class="grow">
          {{ allowedLabel(b.kind) }}
          <input v-model="b.allowedText" type="text" placeholder="e.g. 123456789" />
        </label>
        <label class="grow">
          Model (empty: the router chooses)
          <input v-model="b.model" type="text" placeholder="auto" />
        </label>
      </div>
      <div class="row">
        <span class="grow muted small">{{ statusOf(b.name) }}</span>
        <button class="btn" type="button" :disabled="busy" @click="remove(i)">Remove</button>
      </div>
    </div>

    <div class="row">
      <button class="btn" type="button" :disabled="busy" @click="add">Add a bot</button>
      <button class="btn primary" type="button" :disabled="busy" @click="save">Save bots</button>
    </div>
  </section>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { allowedLabel, newBot, parseChats, SERVICES, type BotSetting } from '@/bots'
import { call } from '@/transport'

/** A bot as the form edits it: the settings' fields, with the chat ids as text. */
type Row = BotSetting & { allowedText: string }

const bots = ref<Row[]>([])
const status = ref<any[]>([])
const busy = ref(false)
const error = ref('')
const message = ref('')
let timer: number | undefined

function tokenHint(b: Row): string {
  if (b.token === '(set)') return 'Saved. Type a new token to replace it'
  return b.kind === 'discord' ? 'From the Discord developer portal (Bot page)' : 'From @BotFather'
}

function rowOf(b: BotSetting): Row {
  return { ...b, allowedText: (b.allowed_chats || []).join(', ') }
}

async function load() {
  try {
    const cfg = await call('config.get', {})
    bots.value = ((cfg.bots || []) as BotSetting[]).map(rowOf)
    status.value = (await call('bots.status', {})) as any[]
  } catch (e: any) {
    error.value = `The bots did not load: ${e?.message || e}`
  }
}

function statusOf(name: string): string {
  const s = status.value.find((x) => x.name === name)
  if (!s) return 'Not saved yet.'
  if (s.running) {
    const counts = `${s.answered} answered, ${s.refused} refused`
    return s.error ? `Running (${counts}). Last error: ${s.error}` : `Running (${counts}).`
  }
  return s.enabled ? 'Starting, or stopped with an error.' : 'Off.'
}

function add() {
  bots.value.push(rowOf(newBot(bots.value)))
}

function remove(i: number) {
  bots.value.splice(i, 1)
}

async function save() {
  error.value = ''
  message.value = ''
  busy.value = true
  try {
    const list = bots.value.map(({ allowedText, ...b }) => ({ ...b, allowed_chats: parseChats(allowedText) }))
    await call('config.set', { settings: { bots: list } })
    message.value = 'Saved. The bots that are on start now; the others stop.'
    await load()
  } catch (e: any) {
    error.value = `Not saved: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

async function check(b: Row) {
  error.value = ''
  message.value = ''
  busy.value = true
  try {
    const r = await call('bots.check', { bot: b.name })
    message.value = `The token works: the bot is @${r.username}.`
  } catch (e: any) {
    error.value = `The token did not work: ${e?.message || e}`
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  load()
  timer = window.setInterval(async () => {
    try {
      status.value = (await call('bots.status', {})) as any[]
    } catch (_) {
      /* the hub is gone for a moment */
    }
  }, 5000)
})
onBeforeUnmount(() => window.clearInterval(timer))
</script>

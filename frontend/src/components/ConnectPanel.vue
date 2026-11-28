<template>
  <section class="card" aria-label="Use ModelMistress from other software">
    <h3>Use ModelMistress from other software</h3>
    <p class="muted small">
      Hermes Agent, VS Code, OpenCode, OpenClaw and other tools can use this hub as a model provider, the way they use
      Ollama or LM Studio. Each tool gets its own key, which can be revoked on its own.
    </p>

    <div class="row">
      <label class="grow">
        Tool name
        <input v-model="peer" type="text" maxlength="64" placeholder="opencode" />
      </label>
      <button class="btn primary" type="button" :disabled="!peer.trim() || making" @click="makeKey">Make a key</button>
    </div>
    <p v-if="keyError" class="error-box" role="alert">{{ keyError }}</p>
    <p v-if="madeKey" class="muted small" role="status">
      The key for {{ madeFor }} is <code>{{ madeKey }}</code>. It is shown once: copy it now. It is in the snippets below.
    </p>

    <div class="row">
      <label class="grow">
        Model
        <select v-model="model" :disabled="!models.length">
          <option v-if="!models.length" value="">no model is installed</option>
          <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
        </select>
      </label>
    </div>
    <p class="muted small">Address the other software connects to: <code>{{ baseUrl }}</code>. A tool on another machine needs the hub to listen on the network (the host option), and a key.</p>

    <details v-for="s in snippets" :key="s.title" class="connect-tool">
      <summary>{{ s.title }}</summary>
      <p class="muted small">{{ s.note }}</p>
      <pre class="snippet">{{ s.text }}</pre>
      <button class="btn" type="button" @click="copy(s.text)">Copy</button>
    </details>
    <p v-if="copied" class="muted small" role="status">{{ copied }}</p>

    <DevicesPanel />
    <BotsPanel />
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { envConfig, continueConfig, hermesConfig, ollamaBase, openClawConfig, openCodeConfig } from '@/connect'
import BotsPanel from '@/components/BotsPanel.vue'
import DevicesPanel from '@/components/DevicesPanel.vue'
import { isChatModel, listModels } from '@/chat/models'
import { call, hubInfo } from '@/transport'

const peer = ref('opencode')
const making = ref(false)
const keyError = ref('')
const madeKey = ref('')
const madeFor = ref('')
const model = ref('')
const models = ref<string[]>([])
const baseUrl = ref(window.location.origin)
const copied = ref('')

/** The key is kept only for this page: it is shown once, and the snippets carry it. */
const keyText = computed(() => madeKey.value || '<make a key above>')
const modelText = computed(() => model.value || '<model id>')
const target = computed(() => ({ baseUrl: baseUrl.value, key: keyText.value, model: modelText.value }))

const snippets = computed(() => {
  const t = target.value
  const env = envConfig(t)
  return [
    {
      title: 'OpenCode CLI',
      note: 'Save as opencode.json in a project, or as ~/.config/opencode/opencode.json for every project. Then pick the model with /models.',
      text: openCodeConfig(t),
    },
    {
      title: 'VS Code and JetBrains (Continue)',
      note: 'Save as ~/.continue/config.yaml (the gear icon in the Continue sidebar opens it), then reload the window.',
      text: continueConfig(t),
    },
    {
      title: 'Hermes Agent',
      note: 'Run `hermes model` and choose the custom endpoint with the address and key above, or put this in ~/.hermes/config.yaml. A GitHub issue reports that a custom provider in the Docker image fell back to OpenRouter, so check which model answers.',
      text: hermesConfig(t),
    },
    {
      title: 'OpenClaw',
      note: 'Merge this into ~/.openclaw/openclaw.json under models.providers. The list of models for the provider is not in this snippet; the OpenClaw custom-providers page shows its shape.',
      text: openClawConfig(t),
    },
    {
      title: 'Any OpenAI SDK or tool that reads the standard variables',
      note: 'Set these in the shell the program runs from.',
      text: `${env.bash}\n\n# PowerShell\n${env.powershell}`,
    },
    {
      title: 'Tools that expect an Ollama server',
      note: 'The Ollama-compatible API is on the same address, without /v1.',
      text: ollamaBase(baseUrl.value),
    },
  ]
})

async function makeKey() {
  keyError.value = ''
  making.value = true
  try {
    const name = peer.value.trim()
    const made = await call('key.create', { peer: name, models: [] })
    madeKey.value = made.key
    madeFor.value = name
  } catch (e: any) {
    keyError.value = `The key was not made: ${e?.message || e}`
  } finally {
    making.value = false
  }
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    copied.value = 'Copied.'
  } catch (_) {
    copied.value = 'Could not reach the clipboard; select the text and copy it by hand.'
  }
}

onMounted(async () => {
  try {
    const info = await hubInfo()
    baseUrl.value = info.url.replace(/\/+$/, '')
  } catch (_) {
    /* the page's own address is the hub's address in a browser */
  }
  try {
    models.value = (await listModels()).filter(isChatModel).map((m) => m.id)
    model.value = models.value[0] ?? ''
  } catch (_) {
    /* the list is empty until the hub answers */
  }
})

</script>

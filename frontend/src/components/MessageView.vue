<template>
  <article class="msg" :class="[msg.role, { err: !!msg.error }]" :aria-label="msg.role === 'user' ? 'You' : 'Reply'">
    <header class="who">
      <strong>{{ msg.role === 'user' ? 'You' : msg.model || 'Assistant' }}</strong>
      <span class="grow"></span>
      <button v-if="msg.role === 'assistant' && msg.content" class="mini" type="button" @click="copyAll">{{ copied === 'all' ? 'Copied' : 'Copy' }}</button>
      <button v-if="canRetry" class="mini" type="button" @click="emit('retry')">Regenerate</button>
    </header>

    <div v-if="msg.images?.length" class="imgs">
      <img v-for="(u, i) in msg.images" :key="i" :src="u" alt="attached picture" />
    </div>

    <details v-if="parts.reasoning" class="think" :open="streaming && parts.thinking && !parts.answer">
      <summary>{{ parts.thinking && streaming ? 'Thinking...' : 'Reasoning' }} <span class="muted small">({{ parts.reasoning.length }} characters)</span></summary>
      <div class="thinktext">{{ parts.reasoning }}</div>
    </details>

    <div v-if="msg.role === 'user'" class="usertext">{{ msg.content }}</div>
    <!-- eslint-disable-next-line vue/no-v-html -->
    <div v-else-if="parts.answer" class="md" @click="onClick" v-html="html"></div>
    <p v-else-if="streaming && !msg.error" class="muted wait">{{ waiting }}</p>

    <p v-if="msg.error" class="error-box" role="alert">{{ msg.error }}</p>
    <p v-if="msg.stopped" class="muted small">Stopped.</p>

    <footer v-if="msg.role === 'assistant' && (msg.stats || msg.traceId)" class="stats muted small">
      <template v-if="msg.stats">
        <span v-if="msg.stats.ttft_ms != null" title="time to first token">{{ sec(msg.stats.ttft_ms) }} to first token</span>
        <span v-if="msg.stats.completion_tokens != null">{{ msg.stats.completion_tokens }} tokens</span>
        <span v-if="msg.stats.tps != null">{{ msg.stats.tps }} tok/s</span>
        <span v-if="msg.stats.prompt_tokens != null">{{ msg.stats.prompt_tokens }} prompt tokens</span>
        <span>{{ sec(msg.stats.total_ms) }} total</span>
      </template>
      <router-link v-if="msg.traceId" :to="`/traces/${msg.traceId}`">Trace of this reply</router-link>
      <router-link v-if="msg.traceId" to="/thoughts">Thoughts</router-link>
    </footer>
  </article>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { renderMarkdown, splitThink } from '@/chat/markdown'
import type { Msg } from '@/chat/store'

const props = defineProps<{ msg: Msg; streaming?: boolean; canRetry?: boolean; waiting?: string }>()
const emit = defineEmits<{ (e: 'retry'): void }>()

const parts = computed(() => {
  const m = props.msg
  const t = splitThink(m.content || '')
  const reasoning = [m.reasoning || '', t.reasoning].filter(Boolean).join('\n')
  return { reasoning, answer: t.answer, thinking: t.thinking || (!!m.reasoning && !t.answer) }
})
const html = computed(() => renderMarkdown(parts.value.answer))
const sec = (ms: number) => (ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(ms < 10000 ? 2 : 1)} s`)

const copied = ref('')
async function copy(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch (_) {
    const t = document.createElement('textarea')
    t.value = text
    t.style.position = 'fixed'
    t.style.opacity = '0'
    document.body.appendChild(t)
    t.select()
    const ok = document.execCommand('copy')
    t.remove()
    return ok
  }
}
async function copyAll() {
  if (await copy(parts.value.answer)) {
    copied.value = 'all'
    setTimeout(() => (copied.value = ''), 1200)
  }
}
async function onClick(e: MouseEvent) {
  const b = (e.target as HTMLElement).closest('[data-copy]') as HTMLElement | null
  if (!b) return
  const code = b.closest('.codeblock')?.querySelector('code')?.textContent || ''
  if (await copy(code)) {
    b.textContent = 'Copied'
    setTimeout(() => (b.textContent = 'Copy'), 1200)
  }
}
</script>

<style scoped>
.msg { padding: 10px 14px; border-radius: 12px; border: 1px solid var(--border); background: var(--panel); max-width: 100%; }
.msg.user { background: var(--panel-2); }
.msg.err { border-color: var(--bad); }
.who { display: flex; align-items: center; gap: 6px; margin-bottom: 4px; font-size: 0.85rem; color: var(--muted); }
.mini { background: none; border: 1px solid var(--border); border-radius: 6px; padding: 1px 8px; font-size: 0.75rem; color: var(--muted); }
.mini:hover { border-color: var(--accent); color: var(--text); }
.usertext { white-space: pre-wrap; word-break: break-word; }
.imgs { display: flex; gap: 6px; flex-wrap: wrap; margin-bottom: 6px; }
.imgs img { max-height: 140px; max-width: 100%; border-radius: 8px; border: 1px solid var(--border); }
.think { margin: 4px 0 8px; border: 1px dashed var(--border); border-radius: 8px; padding: 4px 10px; color: var(--muted); }
.think summary { cursor: pointer; font-size: 0.85rem; }
.thinktext { white-space: pre-wrap; word-break: break-word; font-size: 0.85rem; margin-top: 6px; max-height: 320px; overflow-y: auto; }
.wait { font-size: 0.9rem; }
.stats { display: flex; gap: 4px 14px; flex-wrap: wrap; margin-top: 8px; padding-top: 6px; border-top: 1px solid var(--border); }
.md { word-break: break-word; }
.md :deep(p) { margin: 0 0 0.6em; }
.md :deep(p:last-child) { margin-bottom: 0; }
.md :deep(h3), .md :deep(h4), .md :deep(h5), .md :deep(h6) { margin: 0.8em 0 0.3em; font-size: 1.05em; }
.md :deep(h3) { font-size: 1.2em; }
.md :deep(ul), .md :deep(ol) { margin: 0 0 0.6em 1.4em; }
.md :deep(blockquote) { margin: 0 0 0.6em; padding-left: 10px; border-left: 3px solid var(--border); color: var(--muted); }
.md :deep(hr) { border: 0; border-top: 1px solid var(--border); margin: 0.8em 0; }
.md :deep(code) { font-family: var(--mono); font-size: 0.88em; background: var(--panel-2); padding: 1px 5px; border-radius: 5px; }
.md :deep(.codeblock) { margin: 0.4em 0 0.8em; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; background: var(--bg); }
.md :deep(.cbhead) { display: flex; justify-content: space-between; align-items: center; padding: 2px 10px; background: var(--panel-2); font-size: 0.75rem; color: var(--muted); }
.md :deep(.cbcopy) { background: none; border: 0; color: var(--muted); font-size: 0.75rem; padding: 2px 4px; }
.md :deep(.cbcopy:hover) { color: var(--text); }
.md :deep(pre) { margin: 0; padding: 10px 12px; overflow-x: auto; }
.md :deep(pre code) { background: none; padding: 0; font-size: 0.85rem; white-space: pre; }
.md :deep(.tablewrap) { overflow-x: auto; margin-bottom: 0.6em; }
.md :deep(table) { border-collapse: collapse; font-size: 0.9em; }
.md :deep(th), .md :deep(td) { border: 1px solid var(--border); padding: 3px 8px; text-align: left; }
</style>

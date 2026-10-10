<template>
  <div class="shell">
    <header class="top">
      <strong class="brand">ModelMistress</strong>
      <nav>
        <router-link to="/thoughts">Thoughts</router-link>
        <router-link to="/traces">Traces</router-link>
        <router-link to="/overview">Overview</router-link>
        <router-link to="/models">Models</router-link>
        <router-link to="/swarm">Swarm</router-link>
        <router-link to="/settings">Settings</router-link>
      </nav>
      <span class="grow"></span>
      <button class="btn" :title="`Switch to ${dark ? 'light' : 'dark'}`" @click="toggle">{{ dark ? 'Light' : 'Dark' }}</button>
      <button v-if="!tauri && auth.token" class="btn" @click="signOut()">Sign out</button>
    </header>
    <main class="content">
      <TokenGate v-if="gate" />
      <router-view v-else />
    </main>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import TokenGate from '@/components/TokenGate.vue'
import { auth, inTauri, signOut } from '@/transport'

const tauri = inTauri()
const gate = computed(() => !tauri && !auth.token)

const stored = localStorage.getItem('mm_theme')
const dark = ref(stored ? stored === 'dark' : window.matchMedia?.('(prefers-color-scheme: dark)').matches)
const apply = () => {
  document.documentElement.dataset.theme = dark.value ? 'dark' : 'light'
}
apply()
const toggle = () => {
  dark.value = !dark.value
  localStorage.setItem('mm_theme', dark.value ? 'dark' : 'light')
  apply()
}
</script>

<style scoped>
.shell { display: flex; flex-direction: column; height: 100%; }
.top {
  display: flex; align-items: center; gap: 12px; padding: 8px 16px; background: var(--panel);
  border-bottom: 1px solid var(--border); flex: none;
}
.brand { white-space: nowrap; }
nav { display: flex; gap: 2px; overflow-x: auto; scrollbar-width: none; }
nav a {
  padding: 6px 12px; border-radius: 8px; color: var(--muted); text-decoration: none; white-space: nowrap;
}
nav a.router-link-active { background: var(--panel-2); color: var(--text); font-weight: 600; }
.grow { flex: 1 1 auto; }
.content { flex: 1 1 auto; overflow-y: auto; }
@media (max-width: 640px) {
  .top { flex-wrap: wrap; gap: 6px; padding: 8px 10px; }
  nav { order: 3; width: 100%; }
}
</style>

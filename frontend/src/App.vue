<template>
  <div class="shell">
    <a href="#main" class="skip" @click.prevent="focusMain">Skip to content</a>
    <header class="top">
      <strong class="brand">ModelMistress</strong>
      <nav aria-label="main">
        <router-link v-for="g in groups" :key="g.to" :to="g.to" active-class="x" exact-active-class="x" :class="{ 'router-link-active': g === active }" :aria-current="g === active ? 'page' : undefined">{{ g.label }}</router-link>
      </nav>
      <span class="grow"></span>
      <button class="btn" :title="`Switch to ${dark ? 'light' : 'dark'}`" @click="toggle">{{ dark ? 'Light' : 'Dark' }}</button>
      <button v-if="!tauri && auth.token" class="btn" @click="signOut()">Sign out</button>
    </header>
    <nav v-if="active && active.pages.length > 1" class="sub" :aria-label="`${active.label} pages`">
      <router-link v-for="p in active.pages" :key="p.to" :to="p.to" active-class="x" exact-active-class="x" :class="{ 'router-link-active': p === activePage }">{{ p.label }}</router-link>
    </nav>
    <main id="main" class="content">
      <TokenGate v-if="gate" />
      <router-view v-else />
    </main>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { useRoute } from 'vue-router'
import TokenGate from '@/components/TokenGate.vue'
import { auth, inTauri, signOut } from '@/transport'

// Pages grouped by what they are for; the group row is always shown, the pages of the current group under it.
const groups = [
  { label: 'Thoughts', to: '/thoughts', pages: [{ label: 'Thoughts', to: '/thoughts' }] },
  { label: 'Traces', to: '/traces', pages: [{ label: 'Traces', to: '/traces' }] },
  { label: 'Knowledge', to: '/knowledge', pages: [{ label: 'Library', to: '/knowledge' }, { label: 'Residency', to: '/residency' }, { label: 'Vocabulary', to: '/vocabulary' }] },
  { label: 'Data', to: '/data', pages: [{ label: 'Data studio', to: '/data' }] },
  { label: 'Factory', to: '/factory', pages: [{ label: 'Tasks', to: '/factory' }, { label: 'Review queue', to: '/factory/review' }] },
  { label: 'Models', to: '/models', pages: [{ label: 'Models', to: '/models' }, { label: 'Overview', to: '/overview' }, { label: 'Swarm', to: '/swarm' }] },
  { label: 'Settings', to: '/settings', pages: [{ label: 'Settings', to: '/settings' }] },
]
const route = useRoute()
const within = (path: string, base: string) => path === base || path.startsWith(base + '/')
const active = computed(() => groups.find((g) => g.pages.some((p) => within(route.path, p.to))))
const activePage = computed(() =>
  [...(active.value?.pages || [])].sort((a, b) => b.to.length - a.to.length).find((p) => within(route.path, p.to)),
)
const focusMain = () => {
  const m = document.getElementById('main')
  m?.setAttribute('tabindex', '-1')
  m?.focus()
}

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
.sub { display: flex; gap: 2px; padding: 4px 16px; background: var(--panel); border-bottom: 1px solid var(--border); overflow-x: auto; flex: none; }
.sub a { padding: 3px 10px; border-radius: 6px; color: var(--muted); text-decoration: none; font-size: 0.9rem; white-space: nowrap; }
.sub a.router-link-active { color: var(--text); font-weight: 600; background: var(--panel-2); }
.grow { flex: 1 1 auto; }
.content { flex: 1 1 auto; overflow-y: auto; }
@media (max-width: 640px) {
  .top { flex-wrap: wrap; gap: 6px; padding: 8px 10px; }
  nav { order: 3; width: 100%; }
}
</style>

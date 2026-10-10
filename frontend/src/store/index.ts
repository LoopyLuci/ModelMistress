import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import { call, inTauri } from '@/transport'

export const useStore = defineStore('modelMistress', () => {
  const models = ref<any[]>([])
  const agents = ref<any[]>([])
  const settings = ref({
    theme: 'dark' as 'dark' | 'light',
    language: 'en',
    notifications: true,
    autoSave: true
  })
  
  // Everything goes through the transport: the desktop shell (desktop-app/src/main.rs commands), or fetch to the hub
  // when this page is served by it (see ../transport.ts).
  const loadModels = async (refresh = false) => {
    models.value = inTauri()
      ? await (window as any).__TAURI__.tauri.invoke('hub_models', { refresh })
      : await call('model.list', { refresh })
  }

  const callHub = (op: string, args: Record<string, unknown> = {}) => call(op, args)

  const addAgent = (agent: any) => {
    agents.value.push(agent)
  }
  
  const toggleTheme = () => {
    settings.value.theme = settings.value.theme === 'dark' ? 'light' : 'dark'
  }
  
  return {
    models,
    agents,
    settings,
    loadModels,
    callHub,
    addAgent,
    toggleTheme
  }
})
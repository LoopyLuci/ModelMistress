import { defineStore } from 'pinia'
import { ref, computed } from 'vue'

export const useStore = defineStore('modelMistress', () => {
  const models = ref<any[]>([])
  const agents = ref<any[]>([])
  const settings = ref({
    theme: 'dark' as 'dark' | 'light',
    language: 'en',
    notifications: true,
    autoSave: true
  })
  
  // Everything goes through the desktop shell to ModelMistress's hub (src-tauri commands in desktop-app/src/main.rs).
  const hub = (cmd: string, args: Record<string, unknown> = {}) => {
    const tauri = (window as any).__TAURI__
    if (!tauri) return Promise.reject('not running inside the ModelMistress desktop app')
    return tauri.tauri.invoke(cmd, args)
  }

  const loadModels = async (refresh = false) => {
    models.value = await hub('hub_models', { refresh })
  }

  const callHub = (op: string, args: Record<string, unknown> = {}) => hub('hub_call', { op, args })
  
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
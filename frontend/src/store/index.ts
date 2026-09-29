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
  
  const loadModels = async () => {
    models.value = []
  }
  
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
    addAgent,
    toggleTheme
  }
})
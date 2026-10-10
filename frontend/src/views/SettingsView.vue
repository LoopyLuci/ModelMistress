<template>
  <div class="settings">
    <h2>Settings</h2>
    
    <div class="settings-section">
      <h3>Appearance</h3>
      <div class="setting-item">
        <label>Theme:</label>
        <div class="theme-options">
          <button 
            v-for="theme in themes" 
            :key="theme.value"
            :class="['theme-btn', settings.theme === theme.value ? 'active' : '']"
            @click="settings.theme = theme.value"
          >
            {{ theme.label }}
          </button>
        </div>
      </div>
    </div>
    
    <div class="settings-section">
      <h3>Notifications</h3>
      <div class="setting-item">
        <label>
          <input type="checkbox" v-model="settings.notifications">
          Enable notifications
        </label>
      </div>
    </div>
    
    <div class="settings-section">
      <h3>Paths</h3>
      <div class="setting-item">
        <label>Model Directory:</label>
        <input 
          type="text" 
          v-model="settings.modelPath" 
          placeholder="/path/to/models"
        />
      </div>
    </div>
    
    <div class="settings-section">
      <h3>Hub</h3>
      <div class="setting-item">
        <label>
          The app talks to ModelMistress's own hub (found through its control file, or started inside the app).
          Models, downloads, chat, speech and peer keys all go through it.
        </label>
      </div>
      <div class="setting-item">
        <label>Status:</label>
        <span>{{ hubStatus }}</span>
      </div>
    </div>
    
    <div class="settings-section">
      <h3>Advanced</h3>
      <div class="setting-item">
        <label>
          <input type="checkbox" v-model="settings.autoSave">
          Auto-save session state
        </label>
      </div>
      <div class="setting-item">
        <label>
          <input type="checkbox" v-model="settings.enableTelemetry">
          Enable usage analytics
        </label>
      </div>
    </div>
    
    <div class="settings-actions">
      <button @click="saveSettings" class="btn btn-primary">Save Settings</button>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue'
import { useStore } from '@/store'

const store = useStore()
const settings = ref({
  theme: 'dark',
  notifications: true,
  autoSave: true,
  modelPath: '',
  enableTelemetry: false
})

const hubStatus = ref('checking...')

onMounted(async () => {
  try {
    await store.loadModels()
    hubStatus.value = `connected, ${store.models.length} model(s)`
  } catch (e) {
    hubStatus.value = `not reachable: ${e}`
  }
})

const themes = [
  { value: 'dark', label: 'Dark' },
  { value: 'light', label: 'Light' }
]

const saveSettings = () => {
  console.log('Settings saved:', settings.value)
}
</script>

<style scoped>
.settings {
  padding: 24px;
}

.settings-section {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 20px;
  margin-bottom: 16px;
}

.settings-section h3 {
  margin: 0 0 16px 0;
  color: #00d9ff;
}

.setting-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.setting-item:last-child {
  border-bottom: none;
}

.setting-item label {
  display: flex;
  align-items: center;
  gap: 8px;
  color: #ccc;
}

.setting-item input[type="text"] {
  padding: 8px 12px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.2);
  background: #1a1a2e;
  color: #fff;
  width: 200px;
}

.setting-item input[type="checkbox"] {
  width: auto;
}

.theme-options {
  display: flex;
  gap: 8px;
}

.theme-btn {
  padding: 6px 16px;
  border-radius: 20px;
  border: none;
  background: rgba(255, 255, 255, 0.1);
  color: #ccc;
  cursor: pointer;
  transition: all 0.2s;
}

.theme-btn.active {
  background: linear-gradient(90deg, #00d9ff, #00ff88);
  color: #000;
}

.settings-actions {
  margin-top: 24px;
  text-align: center;
}
</style>
<template>
  <div class="models">
    <div class="models-header">
      <h2>Model Management</h2>
      <button @click="scanDirectory" class="btn btn-primary">
        📁 Scan Directory
      </button>
    </div>
    
    <div class="models-grid">
      <div v-for="model in models" :key="model.id" class="model-card">
        <div class="model-header">
          <h3>{{ model.name }}</h3>
          <span class="model-status" :class="model.loaded ? 'loaded' : 'ready'">{{ model.loaded ? 'Loaded' : 'Ready' }}</span>
        </div>
        <div class="model-info">
          <p>Type: {{ model.type }}</p>
          <p>Size: {{ model.size }} MB</p>
          <p>Context: {{ model.context }} tokens</p>
        </div>
        <div class="model-actions">
          <button v-if="!model.loaded" @click="loadModel(model)" class="btn btn-success">Load</button>
          <button v-else @click="unloadModel(model)" class="btn btn-danger">Unload</button>
        </div>
      </div>
    </div>
    
    <div v-if="scannerActive" class="scanner-overlay">
      <div class="scanner">
        <p>⏳ Scanning for models...</p>
        <div class="spinner"></div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref, computed } from 'vue'
import { useStore } from '@/store'

const store = useStore()

const models = ref([
  {
    id: 1,
    name: 'Qwythos-9B-Claude-Mythos',
    type: 'Q5_K',
    size: 5120,
    context: 8192,
    loaded: false,
    path: 'E:/AIModels/Qwythos-9B'
  },
  {
    id: 2,
    name: 'gemma:2b',
    type: 'Q4_0',
    size: 1200,
    context: 2048,
    loaded: false,
    path: '/models/gemma-2b.gguf'
  }
])

const scannerActive = ref(false)

const scanDirectory = async () => {
  scannerActive.value = true
  // Simulate scanning
  await new Promise(resolve => setTimeout(resolve, 1500))
  scannerActive.value = false
  
  // Add new found models
  models.value.push({
    id: models.value.length + 1,
    name: 'Newly Found Model',
    type: 'Q8_0',
    size: 4500,
    context: 4096,
    loaded: false,
    path: '/new-model.gguf'
  })
}

const loadModel = (model) => {
  model.loaded = true
}

const unloadModel = (model) => {
  model.loaded = false
}
</script>

<style scoped>
.models {
  padding: 24px;
}

.models-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 24px;
}

.models-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
  gap: 16px;
}

.model-card {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 20px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.model-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 16px;
}

.model-status {
  padding: 4px 12px;
  border-radius: 20px;
  font-size: 0.8rem;
  font-weight: 500;
}

.model-status.loaded {
  background: rgba(0, 255, 136, 0.2);
  color: #00ff88;
}

.model-status.ready {
  background: rgba(136, 136, 136, 0.2);
  color: #888;
}

.model-info p {
  margin: 4px 0;
  color: #888;
  font-size: 0.9rem;
}

.model-actions {
  margin-top: 16px;
  padding-top: 16px;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}

.btn {
  padding: 8px 16px;
  border-radius: 6px;
  border: none;
  cursor: pointer;
  font-size: 0.9rem;
  transition: all 0.2s;
}

.btn-primary {
  background: linear-gradient(90deg, #00d9ff, #00ff88);
  color: #000;
}

.btn-success {
  background: rgba(0, 255, 136, 0.2);
  color: #00ff88;
}

.btn-danger {
  background: rgba(255, 107, 107, 0.2);
  color: #ff6b6b;
}

.scanner-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.8);
  display: flex;
  justify-content: center;
  align-items: center;
  z-index: 1000;
}

.scanner {
  text-align: center;
  color: #fff;
}

.spinner {
  width: 40px;
  height: 40px;
  border: 3px solid rgba(255, 255, 255, 0.3);
  border-top-color: #00d9ff;
  border-radius: 50%;
  animation: spin 1s linear infinite;
  margin: 16px auto;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}
</style>
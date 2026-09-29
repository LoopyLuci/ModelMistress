<template>
  <div class="dashboard">
    <div class="stats-grid">
      <div class="stat-card">
        <div class="stat-icon">🧠</div>
        <div class="stat-info">
          <h3>Models Loaded</h3>
          <p class="stat-value">{{ loadedModels }}</p>
        </div>
      </div>
      
      <div class="stat-card">
        <div class="stat-icon">🤖</div>
        <div class="stat-info">
          <h3>Agents Active</h3>
          <p class="stat-value">{{ activeAgents }}</p>
        </div>
      </div>
      
      <div class="stat-card">
        <div class="stat-icon">⚡</div>
        <div class="stat-info">
          <h3>Status</h3>
          <p class="stat-value" :class="statusClass">{{ status }}</p>
        </div>
      </div>
    </div>
    
    <div class="section">
      <h2>Recently Loaded Models</h2>
      <div class="model-list">
        <div v-for="model in recentModels" :key="model.id" class="model-item">
          <span class="model-name">{{ model.name }}</span>
          <span class="model-status">{{ model.status }}</span>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue'
import { useStore } from '@/store'

const store = useStore()

const loadedModels = computed(() => store.models.length)
const activeAgents = computed(() => store.agents.length)
const status = computed(() => store.models.length > 0 ? 'online' : 'offline')
const statusClass = computed(() => status.value === 'online' ? 'status-online' : 'status-offline')

const recentModels = computed(() => [
  { id: 1, name: 'Qwythos-9B', status: 'loaded' },
  { id: 2, name: 'gemma:2b', status: 'ready' }
])
</script>

<style scoped>
.dashboard {
  padding: 24px;
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 16px;
  margin-bottom: 32px;
}

.stat-card {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 20px;
  display: flex;
  align-items: center;
  gap: 12px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.stat-icon {
  font-size: 2rem;
}

.stat-info h3 {
  margin: 0;
  color: #888;
  font-size: 0.8rem;
  font-weight: normal;
}

.stat-value {
  margin: 4px 0 0 0;
  font-size: 1.5rem;
  font-weight: bold;
}

.status-online {
  color: #00ff88;
}

.status-offline {
  color: #ff6b6b;
}

.section h2 {
  margin: 0 0 16px 0;
}

.model-list {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 16px;
}

.model-item {
  display: flex;
  justify-content: space-between;
  padding: 12px 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.model-item:last-child {
  border-bottom: none;
}

.model-name {
  color: #00d9ff;
}

.model-status {
  color: #888;
  font-size: 0.9rem;
}
</style>
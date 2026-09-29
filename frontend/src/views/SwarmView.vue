<template>
  <div class="swarm">
    <div class="swarm-header">
      <h2>Agent Swarm Management</h2>
      <button @click="addAgent" class="btn btn-primary">
        ➕ Add Agent
      </button>
    </div>
    
    <div class="agents-grid">
      <div v-for="agent in agents" :key="agent.id" class="agent-card">
        <div class="agent-header">
          <h3>{{ agent.name }}</h3>
          <span class="agent-status" :class="agent.status">{{ agent.status }}</span>
        </div>
        
        <div class="agent-config">
          <div class="config-group">
            <label>Role:</label>
            <select v-model="agent.role">
              <option value="leaf">Leaf Worker</option>
              <option value="orchestrator">Orchestrator</option>
            </select>
          </div>
          
          <div class="config-group">
            <label>Effort:</label>
            <select v-model="agent.reasoning_effort">
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="max">Max</option>
            </select>
          </div>
          
          <div class="config-group">
            <label>Model:</label>
            <select v-model="agent.model">
              <option value="lmf2.5:8b">lmf2.5:8b</option>
              <option value="Qwythos-9B">Qwythos-9B</option>
              <option value="gemma:2b">gemma:2b</option>
            </select>
          </div>
        </div>
        
        <div class="agent-actions">
          <button @click="removeAgent(agent.id)" class="btn btn-danger">Remove</button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { useStore } from '@/store'

const store = useStore()

const agents = ref([
  {
    id: 1,
    name: 'default-worker',
    role: 'leaf',
    reasoning_effort: 'high',
    model: 'lmf2.5:8b',
    max_actions: 50,
    timeout_seconds: 300,
    tools_enabled: ['terminal', 'file', 'web'],
    status: 'available'
  },
  {
    id: 2,
    name: 'orchestrator',
    role: 'orchestrator',
    reasoning_effort: 'medium',
    model: 'Qwythos-9B',
    max_actions: 100,
    timeout_seconds: 600,
    tools_enabled: ['terminal', 'file', 'web', 'delegation'],
    status: 'available'
  }
])

const nextId = 3

const addAgent = () => {
  agents.value.push({
    id: nextId++,
    name: `agent-${agents.value.length + 1}`,
    role: 'leaf',
    reasoning_effort: 'high',
    model: 'lmf2.5:8b',
    max_actions: 50,
    timeout_seconds: 300,
    tools_enabled: ['terminal', 'file'],
    status: 'available'
  })
}

const removeAgent = (id) => {
  agents.value = agents.value.filter(a => a.id !== id)
}
</script>

<style scoped>
.swarm {
  padding: 24px;
}

.swarm-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 24px;
}

.agents-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(350px, 1fr));
  gap: 16px;
}

.agent-card {
  background: rgba(255, 255, 255, 0.05);
  border-radius: 12px;
  padding: 20px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.agent-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 16px;
}

.agent-status {
  padding: 4px 12px;
  border-radius: 20px;
  font-size: 0.8rem;
  font-weight: 500;
  background: rgba(0, 255, 136, 0.1);
  color: #00ff88;
}

.agent-config {
  display: flex;
  flex-direction: column;
  gap: 12px;
  margin-bottom: 16px;
}

.config-group {
  display: flex;
  align-items: center;
  gap: 8px;
}

.config-group label {
  width: 80px;
  color: #888;
  font-size: 0.9rem;
}

.config-group select {
  flex: 1;
  padding: 8px 12px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.2);
  background: #1a1a2e;
  color: #fff;
  font-size: 0.9rem;
}

.agent-actions {
  padding-top: 16px;
  border-top: 1px solid rgba(255, 255, 255, 0.1);
}
</style>
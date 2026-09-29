<template>
  <div class="model-mistress-app">
    <div class="header">
      <h1>🔮 Model Mistress</h1>
      <p class="subtitle">Enterprise-grade model serving platform</p>
    </div>
    
    <div class="main-content">
      <div class="sidebar">
        <nav>
          <router-link to="/" class="nav-item active">Dashboard</router-link>
          <router-link to="/models" class="nav-item">Models</router-link>
          <router-link to="/swarm" class="nav-item">Swarm</router-link>
          <router-link to="/settings" class="nav-item">Settings</router-link>
        </nav>
      </div>
      
      <div class="content">
        <router-view />
      </div>
    </div>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { useStore } from '@/store'

const store = useStore()
</script>

<style scoped>
.model-mistress-app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
  background: linear-gradient(135deg, #0f0f23 0%, #1a1a2e 100%);
  color: #eee;
}

.header {
  padding: 24px;
  text-align: center;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.header h1 {
  margin: 0;
  font-size: 2rem;
  background: linear-gradient(90deg, #00d9ff, #00ff88);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
  background-clip: text;
}

.subtitle {
  margin: 8px 0 0 0;
  color: #888;
  font-size: 0.9rem;
}
</style>
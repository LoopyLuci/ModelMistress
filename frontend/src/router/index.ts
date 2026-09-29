import { createRouter, createWebHistory } from 'vue-router'
import Dashboard from '../views/Dashboard.vue'
import ModelsView from '../views/ModelsView.vue'
import SwarmView from '../views/SwarmView.vue'
import SettingsView from '../views/SettingsView.vue'

const routes = [
  {
    path: '/',
    name: 'Dashboard',
    component: Dashboard
  },
  {
    path: '/models',
    name: 'Models',
    component: ModelsView
  },
  {
    path: '/swarm',
    name: 'Swarm',
    component: SwarmView
  },
  {
    path: '/settings',
    name: 'Settings',
    component: SettingsView
  }
]

const router = createRouter({
  history: createWebHistory(),
  routes
})

export default router
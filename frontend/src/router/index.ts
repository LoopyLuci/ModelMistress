import { createRouter, createWebHashHistory } from 'vue-router'
import Dashboard from '../views/Dashboard.vue'
import ModelsView from '../views/ModelsView.vue'
import SwarmView from '../views/SwarmView.vue'
import SettingsView from '../views/SettingsView.vue'
import ThoughtsView from '../views/ThoughtsView.vue'
import TracesView from '../views/TracesView.vue'
import TraceDetailView from '../views/TraceDetailView.vue'
import DiffView from '../views/DiffView.vue'

const routes = [
  { path: '/', redirect: '/thoughts' },
  { path: '/thoughts', name: 'Thoughts', component: ThoughtsView },
  { path: '/traces', name: 'Traces', component: TracesView },
  { path: '/traces/:id', name: 'Trace', component: TraceDetailView, props: true },
  { path: '/diff/:a/:b', name: 'Diff', component: DiffView, props: true },
  { path: '/overview', name: 'Dashboard', component: Dashboard },
  { path: '/models', name: 'Models', component: ModelsView },
  { path: '/swarm', name: 'Swarm', component: SwarmView },
  { path: '/settings', name: 'Settings', component: SettingsView },
]

// Hash routing: the hub serves one file at /ui/, and the desktop app has no server to fall back on.
export default createRouter({
  history: createWebHashHistory(),
  routes,
})

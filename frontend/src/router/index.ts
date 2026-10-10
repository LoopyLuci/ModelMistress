import { createRouter, createWebHashHistory } from 'vue-router'
import Dashboard from '../views/Dashboard.vue'
import ModelsView from '../views/ModelsView.vue'
import SwarmView from '../views/SwarmView.vue'
import SettingsView from '../views/SettingsView.vue'
import ThoughtsView from '../views/ThoughtsView.vue'
import TracesView from '../views/TracesView.vue'
import TraceDetailView from '../views/TraceDetailView.vue'
import DiffView from '../views/DiffView.vue'
import KnowledgeView from '../views/KnowledgeView.vue'
import KmDetailView from '../views/KmDetailView.vue'
import ResidencyView from '../views/ResidencyView.vue'
import DataView from '../views/DataView.vue'
import VocabularyView from '../views/VocabularyView.vue'
import FactoryView from '../views/FactoryView.vue'
import FactoryTaskView from '../views/FactoryTaskView.vue'
import FactoryReviewView from '../views/FactoryReviewView.vue'

const routes = [
  { path: '/', redirect: '/thoughts' },
  { path: '/thoughts', name: 'Thoughts', component: ThoughtsView },
  { path: '/traces', name: 'Traces', component: TracesView },
  { path: '/traces/:id', name: 'Trace', component: TraceDetailView, props: true },
  { path: '/diff/:a/:b', name: 'Diff', component: DiffView, props: true },
  { path: '/knowledge', name: 'Knowledge', component: KnowledgeView },
  { path: '/knowledge/:id', name: 'Km', component: KmDetailView, props: true },
  { path: '/residency', name: 'Residency', component: ResidencyView },
  { path: '/vocabulary', name: 'Vocabulary', component: VocabularyView },
  { path: '/data/:id?', name: 'Data', component: DataView, props: true },
  { path: '/factory', name: 'Factory', component: FactoryView },
  { path: '/factory/review', name: 'FactoryReview', component: FactoryReviewView },
  { path: '/factory/:id', name: 'FactoryTask', component: FactoryTaskView, props: true },
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

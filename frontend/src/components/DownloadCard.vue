<template>
  <div class="card">
    <div class="row">
      <strong class="grow name">{{ job.model }}</strong>
      <span class="pill" :class="pillClass">{{ job.status }}</span>
      <button v-if="job.status === 'running'" class="btn" type="button" @click="emit('stop', job.job)">Stop</button>
      <button v-else-if="canRestart(job)" class="btn primary" type="button" @click="emit('restart', job.job)">Restart</button>
    </div>
    <progress v-if="job.status === 'running'" :value="job.percent" max="100" class="prog" :aria-label="`Download progress ${job.percent}%`"></progress>
    <p class="muted small">
      <template v-if="job.status === 'running'">{{ job.percent }}% of {{ fmtBytes(job.total_bytes) }}, {{ job.mb_per_s }} MB/s. </template>
      {{ job.message }}
    </p>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { canRestart, type DownloadJob } from '@/downloads'
import { fmtBytes } from '@/util'

const props = defineProps<{ job: DownloadJob }>()
const emit = defineEmits<{ stop: [id: string]; restart: [id: string] }>()

const pillClass = computed(() =>
  props.job.status === 'done' ? 'good' : props.job.status === 'failed' ? 'bad' : props.job.status,
)
</script>

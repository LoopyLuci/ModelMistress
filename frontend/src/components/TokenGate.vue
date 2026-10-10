<template>
  <div class="gate">
    <form class="card" @submit.prevent="go">
      <h2>Owner token</h2>
      <p class="muted">
        This page talks to the hub with the owner's token. It is kept for this browser tab only and never put in an
        address. The hub writes it to <span class="mono">control.json</span> in its home folder
        (<span class="mono">model-mistress status</span> shows where).
      </p>
      <p v-if="auth.error" class="error-box">{{ auth.error }}</p>
      <input v-model="value" type="password" autocomplete="off" placeholder="paste the token" autofocus />
      <button class="btn primary" :disabled="!value.trim()">Continue</button>
    </form>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { auth, setToken } from '@/transport'

const value = ref('')
const go = () => setToken(value.value)
</script>

<style scoped>
.gate { display: flex; justify-content: center; padding: 48px 12px; }
form { width: min(460px, 100%); display: flex; flex-direction: column; gap: 12px; }
</style>

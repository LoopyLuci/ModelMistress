<template>
  <div class="card" role="status">
    <h3>{{ what }} not available</h3>
    <p>
      This hub does not have the {{ what.toLowerCase() }} yet: it answered that the operation
      <span class="mono">{{ op }}</span> does not exist. The page will work once a hub with it is running.
    </p>
  </div>
</template>

<script setup lang="ts">
defineProps<{ what: string; op: string }>()
</script>

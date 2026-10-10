<template>
  <table class="terms" :aria-label="`score terms of ${hit.key}`">
    <thead><tr><th>term</th><th class="num">raw</th><th class="num">weight</th><th class="num">value</th><th>note</th></tr></thead>
    <tbody>
      <tr>
        <td>vector</td><td class="num">{{ f(t.vec.raw) }}</td><td class="num">{{ f(t.vec.weight) }}</td><td class="num">{{ f(t.vec.value) }}</td>
        <td class="muted">{{ t.vec.used ? 'cosine to the descriptor centroid' : 'not used (no centroid, or another embedding model)' }}</td>
      </tr>
      <tr>
        <td>lexical</td><td class="num">{{ f(t.lex.raw) }}</td><td class="num">{{ f(t.lex.weight) }}</td><td class="num">{{ f(t.lex.value) }}</td>
        <td class="muted">BM25 over name, description, descriptor, keywords</td>
      </tr>
      <tr>
        <td>router</td><td class="num">{{ f(t.route.raw) }}</td><td class="num">{{ f(t.route.weight) }}</td><td class="num">{{ f(t.route.value) }}</td>
        <td class="muted">learned router (0 until a router KM drives it)</td>
      </tr>
      <tr>
        <td>prior</td><td class="num">-</td><td class="num">-</td><td class="num">{{ f(t.prior.value) }}</td><td class="muted">the manifest's prior</td>
      </tr>
      <tr>
        <td>pin</td><td class="num">{{ t.pin.pinned ? 'pinned' : '-' }}</td><td class="num">-</td><td class="num">{{ f(t.pin.value) }}</td>
        <td class="muted">pin bonus</td>
      </tr>
      <tr>
        <td>tags</td><td class="num">{{ f(t.tags.confidence_sum) }}</td><td class="num">{{ f(t.tags.weight) }}</td><td class="num">{{ f(t.tags.value) }}</td>
        <td class="muted">{{ t.tags.matched }} query tag(s) matched, summed by confidence</td>
      </tr>
    </tbody>
    <tfoot><tr><th colspan="3">score</th><th class="num">{{ f(hit.score) }}</th><td class="muted">{{ hit.version_chosen_by }}</td></tr></tfoot>
  </table>
  <p v-for="n in hit.notes" :key="n" class="muted small">{{ n }}</p>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { SearchHit } from '@/api'

const props = defineProps<{ hit: SearchHit }>()
const t = computed(() => props.hit.terms)
const f = (n: number) => (n == null ? '-' : Number(n).toFixed(4))
</script>

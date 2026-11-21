<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    tone?: 'accent' | 'ok' | 'warn' | 'danger' | 'muted'
  }>(),
  { tone: 'muted' },
)

const cls = computed(() => {
  switch (props.tone) {
    case 'accent': return 'pill pill-accent'
    case 'ok': return 'pill pill-ok'
    case 'warn': return 'pill pill-warn'
    case 'danger': return 'pill pill-danger'
    default: return 'pill'
  }
})
</script>

<template>
  <span :class="cls" :data-tone="tone" data-test="pill">
    <slot />
  </span>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    tone?: 'info' | 'warn' | 'danger'
    role?: string
  }>(),
  { tone: 'info', role: 'alert' },
)

const cls = computed(() => {
  switch (props.tone) {
    case 'warn': return 'banner banner-warn'
    case 'danger': return 'banner banner-danger'
    default: return 'banner banner-info'
  }
})
</script>

<template>
  <div :class="cls" :role="role" :data-tone="tone" data-test="alert-banner">
    <slot />
  </div>
</template>

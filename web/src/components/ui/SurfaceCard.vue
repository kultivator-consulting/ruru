<script setup lang="ts">
defineProps<{
  tone?: 'default' | 'muted'
}>()
</script>

<template>
  <div
    :class="tone === 'muted' ? 'surface-card-muted' : 'surface-card'"
    :data-tone="tone || 'default'"
    data-test="surface-card"
  >
    <slot />
  </div>
</template>

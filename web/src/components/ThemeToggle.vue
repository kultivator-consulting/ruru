<script setup lang="ts">
import { computed } from 'vue'

import { useThemeStore } from '@/stores/theme.store'

const theme = useThemeStore()

const label = computed(() => {
  switch (theme.mode) {
    case 'light': return 'light'
    case 'dark': return 'dark'
    case 'system': return 'system'
  }
})

const glyph = computed(() => {
  if (theme.mode === 'system') return '◐'
  return theme.resolved === 'dark' ? '☾' : '☀'
})
</script>

<template>
  <button
    type="button"
    class="btn-ghost text-sm"
    :aria-label="`Toggle theme (currently ${label})`"
    :data-mode="theme.mode"
    data-test="theme-toggle"
    @click="theme.cycle"
  >
    <span aria-hidden="true">{{ glyph }}</span>
    <span class="ml-1 hidden sm:inline">{{ label }}</span>
  </button>
</template>

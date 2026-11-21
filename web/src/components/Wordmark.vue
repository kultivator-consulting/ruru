<script setup lang="ts">
import { RouterLink } from 'vue-router'
</script>

<template>
  <RouterLink to="/" class="flex items-baseline gap-2 no-underline" aria-label="Ruru home">
    <span class="text-gradient text-lg font-semibold tracking-tight">RURU</span>
    <span class="text-[0.7rem] uppercase tracking-[0.2em] text-text-muted">whiteboard</span>
  </RouterLink>
</template>

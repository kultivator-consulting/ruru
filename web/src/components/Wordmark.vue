<script setup lang="ts">
import { RouterLink } from 'vue-router'

import owl from '@/assets/brand/ruru-favicon.svg'
</script>

<template>
  <RouterLink to="/" class="flex items-center gap-2.5 no-underline" aria-label="Ruru home">
    <img :src="owl" alt="" width="32" height="32" class="h-8 w-8 flex-none" />
    <span class="flex items-baseline gap-2">
      <span class="font-display text-xl font-bold tracking-tight text-text-primary">RURU</span>
      <span class="text-[0.7rem] uppercase tracking-[0.2em] text-text-muted">Whiteboard</span>
    </span>
  </RouterLink>
</template>

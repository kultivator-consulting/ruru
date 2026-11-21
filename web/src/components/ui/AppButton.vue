<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    variant?: 'primary' | 'ghost' | 'danger'
    size?: 'sm' | 'md'
    type?: 'button' | 'submit' | 'reset'
    disabled?: boolean
    loading?: boolean
  }>(),
  { variant: 'primary', size: 'md', type: 'button', disabled: false, loading: false },
)

const cls = computed(() => {
  const base = props.variant === 'ghost'
    ? 'btn-ghost'
    : props.variant === 'danger'
      ? 'btn-danger'
      : 'btn-primary'
  const size = props.size === 'sm' ? 'text-xs px-3 py-1' : 'text-sm'
  return `${base} ${size}`
})
</script>

<template>
  <button
    :type="type"
    :class="cls"
    :disabled="disabled || loading"
    :aria-busy="loading || undefined"
    :data-variant="variant"
    :data-size="size"
    data-test="app-button"
  >
    <slot />
  </button>
</template>

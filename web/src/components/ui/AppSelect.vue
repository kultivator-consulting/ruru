<script setup lang="ts">
defineProps<{
  modelValue: string | number | null | undefined
  disabled?: boolean
  required?: boolean
}>()
defineEmits<{
  (e: 'update:modelValue', value: string): void
}>()
</script>

<template>
  <select
    class="select"
    :value="modelValue ?? ''"
    :disabled="disabled"
    :required="required"
    data-test="app-select"
    @change="$emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
  >
    <slot />
  </select>
</template>

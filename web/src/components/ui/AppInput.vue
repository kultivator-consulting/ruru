<script setup lang="ts">
defineProps<{
  modelValue: string | number | null | undefined
  type?: string
  placeholder?: string
  disabled?: boolean
  required?: boolean
  maxlength?: number
}>()
defineEmits<{
  (e: 'update:modelValue', value: string): void
}>()
</script>

<template>
  <input
    class="input"
    :type="type || 'text'"
    :value="modelValue ?? ''"
    :placeholder="placeholder"
    :disabled="disabled"
    :required="required"
    :maxlength="maxlength"
    data-test="app-input"
    @input="$emit('update:modelValue', ($event.target as HTMLInputElement).value)"
  />
</template>

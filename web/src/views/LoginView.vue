<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'

import { ApiError } from '@/api/client'
import owl from '@/assets/brand/ruru-logo.svg'
import AlertBanner from '@/components/ui/AlertBanner.vue'
import AppButton from '@/components/ui/AppButton.vue'
import AppInput from '@/components/ui/AppInput.vue'
import SurfaceCard from '@/components/ui/SurfaceCard.vue'
import { useAuthStore } from '@/stores/auth.store'

const auth = useAuthStore()
const router = useRouter()

const password = ref('')
const error = ref<string | null>(null)
const busy = ref(false)

async function onSubmit(): Promise<void> {
  error.value = null
  busy.value = true
  try {
    await auth.login(password.value)
    void router.replace({ name: 'controls' })
  } catch (e) {
    // The server returns 401 with no detail on a bad password, by design; 429 means this
    // address has had too many failures and is locked out for a while.
    error.value =
      e instanceof ApiError && e.status === 429
        ? 'Too many failed attempts. Wait a few minutes and try again.'
        : 'Incorrect password.'
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="mx-auto mt-16 max-w-sm">
    <SurfaceCard class="p-6">
      <img :src="owl" alt="" width="64" height="64" class="mb-4 h-16 w-16" />
      <h1 class="text-gradient mb-1 text-xl font-semibold">Ruru admin</h1>
      <p class="mb-5 text-sm text-text-muted">Sign in to manage the whiteboard camera.</p>

      <form class="space-y-3" @submit.prevent="onSubmit">
        <AppInput
          v-model="password"
          type="password"
          placeholder="Password"
          required
        />
        <AlertBanner v-if="error" tone="danger">{{ error }}</AlertBanner>
        <AppButton type="submit" :loading="busy" class="w-full">Sign in</AppButton>
      </form>
    </SurfaceCard>
  </div>
</template>

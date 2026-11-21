<script setup lang="ts">
import { computed } from 'vue'
import { RouterLink, RouterView, useRoute, useRouter } from 'vue-router'

import ThemeToggle from '@/components/ThemeToggle.vue'
import Wordmark from '@/components/Wordmark.vue'
import AppButton from '@/components/ui/AppButton.vue'
import { useAuthStore } from '@/stores/auth.store'

const auth = useAuthStore()
const route = useRoute()
const router = useRouter()

const navClass = (name: string) =>
  route.name === name
    ? 'text-text-primary'
    : 'text-text-muted hover:text-text-primary transition-colors'

async function onLogout(): Promise<void> {
  await auth.logout()
  void router.replace({ name: 'login' })
}

const showChrome = computed(() => auth.authenticated)
</script>

<template>
  <div class="min-h-screen">
    <header
      class="sticky top-0 z-10 border-b border-hairline bg-surface/70 backdrop-blur-card"
    >
      <div class="mx-auto flex max-w-5xl items-center justify-between gap-4 px-4 py-3">
        <Wordmark />

        <nav v-if="showChrome" class="flex items-center gap-4 text-sm font-medium">
          <RouterLink to="/" :class="navClass('controls')" class="no-underline">Controls</RouterLink>
          <RouterLink to="/calibration" :class="navClass('calibration')" class="no-underline">
            Calibrate
          </RouterLink>
          <RouterLink to="/keys" :class="navClass('keys')" class="no-underline">Security</RouterLink>
        </nav>

        <div class="flex items-center gap-2">
          <ThemeToggle />
          <AppButton v-if="showChrome" variant="ghost" size="sm" @click="onLogout">
            Log out
          </AppButton>
        </div>
      </div>
    </header>

    <main class="mx-auto max-w-5xl px-4 py-6">
      <RouterView />
    </main>
  </div>
</template>

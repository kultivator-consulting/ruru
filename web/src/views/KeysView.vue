<script setup lang="ts">
import { ref } from 'vue'

import { api } from '@/api/client'
import AlertBanner from '@/components/ui/AlertBanner.vue'
import AppButton from '@/components/ui/AppButton.vue'
import Pill from '@/components/ui/Pill.vue'
import SurfaceCard from '@/components/ui/SurfaceCard.vue'

const confirming = ref(false)
const busy = ref(false)
const error = ref<string | null>(null)
const newKey = ref<string | null>(null)
const copied = ref(false)

async function rotate(): Promise<void> {
  error.value = null
  busy.value = true
  try {
    const res = await api.post<{ key: string }>('/api/keys/rotate')
    newKey.value = res.key
    confirming.value = false
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

async function copy(): Promise<void> {
  if (!newKey.value) return
  await navigator.clipboard.writeText(newKey.value)
  copied.value = true
  window.setTimeout(() => (copied.value = false), 1500)
}

// --- change password ------------------------------------------------------
const current = ref('')
const next = ref('')
const confirmPw = ref('')
const pwBusy = ref(false)
const pwError = ref<string | null>(null)
const pwOk = ref(false)

async function changePassword(): Promise<void> {
  pwError.value = null
  pwOk.value = false
  if (next.value !== confirmPw.value) {
    pwError.value = 'the new passwords do not match'
    return
  }
  if (!next.value) {
    pwError.value = 'the new password must not be empty'
    return
  }
  pwBusy.value = true
  try {
    await api.post('/api/password', { current: current.value, new: next.value })
    pwOk.value = true
    current.value = next.value = confirmPw.value = ''
    window.setTimeout(() => (pwOk.value = false), 2500)
  } catch (e) {
    // A wrong current password comes back as 403 with a specific message.
    pwError.value = e instanceof Error ? e.message : String(e)
  } finally {
    pwBusy.value = false
  }
}
</script>

<template>
  <div class="mx-auto max-w-2xl space-y-4">
    <SurfaceCard class="p-5">
      <h1 class="text-gradient mb-1 text-lg font-semibold">Bearer key</h1>
      <p class="mb-4 text-sm text-text-muted">
        MCP clients authenticate to <span class="font-mono">/mcp</span> with a bearer key.
        Only its hash is stored, so it can only be shown once — at mint or rotate. Rotating
        immediately revokes the previous key.
      </p>

      <AlertBanner v-if="error" tone="danger" class="mb-3">{{ error }}</AlertBanner>

      <!-- The freshly minted key, shown once. -->
      <div v-if="newKey" class="space-y-3">
        <AlertBanner tone="warn">
          This key is shown once. Store it now — it can't be retrieved later.
        </AlertBanner>
        <div class="flex items-center gap-2 rounded-lg border border-hairline bg-surface-muted p-3">
          <code class="flex-1 break-all font-mono text-sm" data-test="new-key">{{ newKey }}</code>
          <AppButton variant="ghost" size="sm" @click="copy">
            {{ copied ? 'Copied' : 'Copy' }}
          </AppButton>
        </div>
        <p class="text-xs text-text-subtle">
          Configure the client with header
          <span class="font-mono">Authorization: Bearer &lt;key&gt;</span>.
        </p>
      </div>

      <!-- Rotate flow. -->
      <div v-else-if="confirming" class="space-y-3">
        <AlertBanner tone="warn">
          Rotating revokes the current key immediately. Any MCP client still using it will
          start getting 401s until reconfigured. Continue?
        </AlertBanner>
        <div class="flex gap-2">
          <AppButton variant="danger" size="sm" :loading="busy" @click="rotate">
            Yes, rotate the key
          </AppButton>
          <AppButton variant="ghost" size="sm" @click="confirming = false">Cancel</AppButton>
        </div>
      </div>

      <div v-else class="flex items-center gap-3">
        <AppButton variant="primary" size="sm" @click="confirming = true">
          Rotate bearer key
        </AppButton>
        <Pill tone="muted">shown once</Pill>
      </div>
    </SurfaceCard>

    <!-- Change password -->
    <SurfaceCard class="p-5">
      <h2 class="text-gradient mb-1 text-lg font-semibold">Change password</h2>
      <p class="mb-4 text-sm text-text-muted">
        The admin password is stored only as an argon2id hash. Changing it needs your current
        password and takes effect on the next login; this session stays valid.
      </p>

      <AlertBanner v-if="pwError" tone="danger" class="mb-3">{{ pwError }}</AlertBanner>
      <AlertBanner v-if="pwOk" tone="info" class="mb-3">Password changed.</AlertBanner>

      <form class="space-y-3" @submit.prevent="changePassword">
        <label class="block text-xs text-text-muted">
          Current password
          <input
            v-model="current"
            type="password"
            autocomplete="current-password"
            class="input mt-1 w-full"
            data-test="pw-current"
          />
        </label>
        <label class="block text-xs text-text-muted">
          New password
          <input
            v-model="next"
            type="password"
            autocomplete="new-password"
            class="input mt-1 w-full"
            data-test="pw-new"
          />
        </label>
        <label class="block text-xs text-text-muted">
          Confirm new password
          <input
            v-model="confirmPw"
            type="password"
            autocomplete="new-password"
            class="input mt-1 w-full"
            data-test="pw-confirm"
          />
        </label>
        <AppButton type="submit" size="sm" :loading="pwBusy" data-test="pw-submit">
          Change password
        </AppButton>
      </form>
    </SurfaceCard>
  </div>
</template>

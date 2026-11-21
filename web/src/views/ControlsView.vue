<script setup lang="ts">
import { storeToRefs } from 'pinia'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'

import AlertBanner from '@/components/ui/AlertBanner.vue'
import AppButton from '@/components/ui/AppButton.vue'
import Pill from '@/components/ui/Pill.vue'
import SurfaceCard from '@/components/ui/SurfaceCard.vue'
import { type Control, useCameraStore } from '@/stores/camera.store'

const camera = useCameraStore()
const { status, controls, error } = storeToRefs(camera)

// Bumped to force the <img> to reconnect the MJPEG stream after enabling.
const previewKey = ref(0)
let poll: number | undefined

// The stream is only requested while the camera is enabled and this view is mounted —
// leaving the page unmounts the <img>, which aborts the request and detaches us from the
// source, so it can idle-close. That is the "open only while someone is looking" posture.
const previewSrc = computed(() =>
  status.value?.enabled ? `/api/preview.mjpeg?k=${previewKey.value}` : null,
)

async function onSet(control: Control, event: Event): Promise<void> {
  const value = Number((event.target as HTMLInputElement).value)
  await camera.setControl(control.name, value)
}

async function toggleEnable(): Promise<void> {
  const next = !status.value?.enabled
  await camera.setEnabled(next)
  if (next) previewKey.value++
  await camera.refresh()
}

onMounted(async () => {
  await camera.refresh()
  previewKey.value++
  poll = window.setInterval(() => void camera.refreshStatus(), 3000)
})

onBeforeUnmount(() => {
  if (poll) clearInterval(poll)
})
</script>

<template>
  <div class="space-y-5">
    <!-- Live preview -->
    <SurfaceCard class="p-4">
      <div class="mb-3 flex items-center justify-between gap-3">
        <h2 class="text-sm font-semibold">Live preview</h2>
        <span class="text-xs text-text-subtle">
          the camera is held open only while this page is open
        </span>
      </div>
      <div
        class="flex aspect-video items-center justify-center overflow-hidden rounded-lg border border-hairline bg-surface-muted"
      >
        <img
          v-if="previewSrc"
          :src="previewSrc"
          alt="live whiteboard preview"
          class="h-full w-full object-contain"
          data-test="preview"
        />
        <span v-else class="text-sm text-text-muted">camera disabled</span>
      </div>
    </SurfaceCard>

    <!-- Status + enable -->
    <SurfaceCard class="p-4">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <div class="flex flex-wrap items-center gap-2">
          <Pill :tone="status?.enabled ? 'ok' : 'muted'">
            {{ status?.enabled ? 'enabled' : 'disabled' }}
          </Pill>
          <Pill :tone="status?.open ? 'accent' : 'muted'">
            {{ status?.open ? 'camera open' : 'closed' }}
          </Pill>
          <Pill tone="muted">{{ (status?.fps ?? 0).toFixed(1) }} fps</Pill>
          <Pill tone="muted">{{ status?.camera_backend ?? '—' }}</Pill>
          <Pill v-if="status && status.subscribers > 0" tone="accent">
            {{ status.subscribers }} watching
          </Pill>
        </div>
        <AppButton
          :variant="status?.enabled ? 'danger' : 'primary'"
          size="sm"
          @click="toggleEnable"
        >
          {{ status?.enabled ? 'Disable camera' : 'Enable camera' }}
        </AppButton>
      </div>
    </SurfaceCard>

    <AlertBanner v-if="error" tone="danger">{{ error }}</AlertBanner>
    <AlertBanner v-if="status?.control_warnings?.length" tone="warn">
      <div class="font-medium">Some locked controls did not apply:</div>
      <ul class="ml-4 list-disc">
        <li v-for="w in status.control_warnings" :key="w" class="font-mono text-xs">{{ w }}</li>
      </ul>
    </AlertBanner>
    <AlertBanner v-if="status?.last_error" tone="warn">
      Source reported: {{ status.last_error }}
    </AlertBanner>

    <!-- Controls -->
    <SurfaceCard class="p-4">
      <div class="mb-3 flex items-center justify-between gap-3">
        <h2 class="text-sm font-semibold">Camera controls</h2>
        <span class="text-xs text-text-subtle">saved as defaults — re-applied on every open</span>
      </div>

      <p v-if="!controls.length" class="text-sm text-text-muted">
        No controls — enable the camera to load them.
      </p>

      <div v-else class="space-y-2.5">
        <div v-for="c in controls" :key="c.name" class="flex items-center gap-3">
          <label :for="`ctl-${c.name}`" class="w-56 shrink-0 truncate font-mono text-xs text-text-muted">
            {{ c.name }}
          </label>
          <input
            :id="`ctl-${c.name}`"
            type="range"
            class="flex-1 accent-accent"
            :min="c.min"
            :max="c.max"
            :step="c.step"
            :value="c.value"
            :disabled="!status?.enabled"
            :data-test="`ctl-${c.name}`"
            @change="onSet(c, $event)"
          />
          <span class="w-14 text-right font-mono text-xs tabular-nums">{{ c.value }}</span>
        </div>
      </div>
    </SurfaceCard>
  </div>
</template>

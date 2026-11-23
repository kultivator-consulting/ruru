<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

import AlertBanner from '@/components/ui/AlertBanner.vue'
import AppButton from '@/components/ui/AppButton.vue'
import SurfaceCard from '@/components/ui/SurfaceCard.vue'
import { usePreviewStream } from '@/composables/usePreviewStream'
import { type Corners, useCalibrationStore } from '@/stores/calibration.store'

const cal = useCalibrationStore()

// A sensible inset quad to start dragging from when nothing is calibrated yet.
const DEFAULT: Corners = [
  [0.15, 0.15],
  [0.85, 0.15],
  [0.85, 0.85],
  [0.15, 0.85],
]
const LABELS = ['TL', 'TR', 'BR', 'BL']

// A plain deep-copy; structuredClone throws on Vue's reactive proxies.
const clone = (c: Corners): Corners => [
  [c[0][0], c[0][1]],
  [c[1][0], c[1][1]],
  [c[2][0], c[2][1]],
  [c[3][0], c[3][1]],
]

const pts = ref<Corners>(clone(DEFAULT))
const aspectW = ref(3)
const aspectH = ref(2)
const stage = ref<HTMLElement | null>(null)
const dewarpUrl = ref<string | null>(null)
const busy = ref(false)
const saved = ref(false)
const error = ref<string | null>(null)

let dragging = -1

onMounted(async () => {
  await cal.load()
  if (cal.corners) pts.value = clone(cal.corners)
  aspectW.value = cal.aspect[0]
  aspectH.value = cal.aspect[1]
  void refreshPreview()
})

onBeforeUnmount(() => {
  if (dewarpUrl.value) URL.revokeObjectURL(dewarpUrl.value)
  window.removeEventListener('pointermove', onMove)
  window.removeEventListener('pointerup', onUp)
})

function startDrag(i: number, e: PointerEvent): void {
  e.preventDefault()
  dragging = i
  window.addEventListener('pointermove', onMove)
  window.addEventListener('pointerup', onUp)
}

function onMove(e: PointerEvent): void {
  if (dragging < 0 || !stage.value) return
  const r = stage.value.getBoundingClientRect()
  const x = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width))
  const y = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height))
  pts.value[dragging] = [round(x), round(y)]
}

async function onUp(): Promise<void> {
  if (dragging < 0) return
  dragging = -1
  window.removeEventListener('pointermove', onMove)
  window.removeEventListener('pointerup', onUp)
  // AC: a drag updates the persisted config AND the dewarped preview.
  await persist()
  await refreshPreview()
}

const round = (v: number) => Math.round(v * 1000) / 1000

async function persist(): Promise<void> {
  error.value = null
  cal.aspect = [aspectW.value, aspectH.value]
  try {
    await cal.save(clone(pts.value))
    saved.value = true
    window.setTimeout(() => (saved.value = false), 1200)
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  }
}

async function refreshPreview(): Promise<void> {
  busy.value = true
  error.value = null
  cal.aspect = [aspectW.value, aspectH.value]
  try {
    const url = await cal.dewarpPreview(clone(pts.value))
    if (dewarpUrl.value) URL.revokeObjectURL(dewarpUrl.value)
    dewarpUrl.value = url
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  } finally {
    busy.value = false
  }
}

async function onAspectChange(): Promise<void> {
  await persist()
  await refreshPreview()
}

async function reset(): Promise<void> {
  pts.value = clone(DEFAULT)
  error.value = null
  try {
    await cal.save(null) // clears calibration → uncalibrated (no-warp)
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e)
  }
  await refreshPreview()
}

// A runtime string, not a build-time asset — Vite must not try to resolve it. Pauses when
// the tab is hidden or after a while, so a forgotten tab doesn't hold the camera open.
const { src: rawPreviewSrc, paused, restart: resumePreview } =
  usePreviewStream('/api/preview.mjpeg')

const polygonPoints = () => pts.value.map((p) => p.join(',')).join(' ')
</script>

<template>
  <div class="space-y-5">
    <SurfaceCard class="p-4">
      <div class="mb-1 flex items-center justify-between gap-3">
        <h1 class="text-gradient text-lg font-semibold">Calibrate the board</h1>
        <span class="text-xs text-text-subtle">
          drag the four corners onto the whiteboard edges
        </span>
      </div>
      <p class="mb-3 text-sm text-text-muted">
        Mark the board corners on the live view, then set the measured width : height so the
        dewarped image isn't stretched. Changes save as you drag.
      </p>
      <AlertBanner v-if="error" tone="danger" class="mb-3">{{ error }}</AlertBanner>

      <div class="grid gap-4 lg:grid-cols-2">
        <!-- Raw preview + draggable corners -->
        <div>
          <div class="mb-2 text-xs font-medium text-text-muted">Raw camera</div>
          <div
            ref="stage"
            class="relative aspect-video select-none overflow-hidden rounded-lg border border-hairline bg-surface-muted"
          >
            <img
              v-if="rawPreviewSrc"
              :src="rawPreviewSrc"
              alt="raw camera preview"
              class="pointer-events-none h-full w-full object-contain"
              draggable="false"
            />
            <div
              v-else-if="paused"
              class="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 text-sm text-text-muted"
            >
              <span>Preview paused to release the camera</span>
              <AppButton size="sm" data-test="resume-preview" @click="resumePreview">
                Resume
              </AppButton>
            </div>
            <svg
              class="pointer-events-none absolute inset-0 h-full w-full"
              viewBox="0 0 1 1"
              preserveAspectRatio="none"
            >
              <polygon
                :points="polygonPoints()"
                fill="rgba(255,150,40,0.12)"
                stroke="rgb(255,150,40)"
                stroke-width="0.005"
              />
            </svg>
            <button
              v-for="(p, i) in pts"
              :key="i"
              class="absolute grid h-6 w-6 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full border-2 border-white bg-accent text-[0.6rem] font-bold text-white shadow-cta"
              :style="{ left: `${p[0] * 100}%`, top: `${p[1] * 100}%`, cursor: 'grab', touchAction: 'none' }"
              :data-test="`corner-${LABELS[i]}`"
              @pointerdown="startDrag(i, $event)"
            >
              {{ LABELS[i] }}
            </button>
          </div>
        </div>

        <!-- Dewarped result -->
        <div>
          <div class="mb-2 flex items-center gap-2 text-xs font-medium text-text-muted">
            Dewarped result
            <span v-if="busy" class="text-text-subtle">updating…</span>
          </div>
          <div
            class="grid aspect-video place-items-center overflow-hidden rounded-lg border border-hairline bg-surface-muted"
          >
            <img
              v-if="dewarpUrl"
              :src="dewarpUrl"
              alt="dewarped preview"
              class="h-full w-full object-contain"
              data-test="dewarped"
            />
            <span v-else class="text-sm text-text-muted">drag a corner to preview</span>
          </div>
        </div>
      </div>

      <div class="mt-4 flex flex-wrap items-end justify-between gap-3">
        <div class="flex items-end gap-2">
          <label class="text-xs text-text-muted">
            Board width
            <input
              v-model.number="aspectW"
              type="number"
              min="1"
              class="input mt-1 w-24"
              @change="onAspectChange"
            />
          </label>
          <span class="pb-2 text-text-muted">:</span>
          <label class="text-xs text-text-muted">
            height
            <input
              v-model.number="aspectH"
              type="number"
              min="1"
              class="input mt-1 w-24"
              @change="onAspectChange"
            />
          </label>
        </div>
        <div class="flex items-center gap-2">
          <span v-if="saved" class="text-xs text-ok">saved</span>
          <AppButton variant="ghost" size="sm" @click="reset">Reset (uncalibrate)</AppButton>
          <AppButton size="sm" @click="persist">Save</AppButton>
        </div>
      </div>
    </SurfaceCard>
  </div>
</template>

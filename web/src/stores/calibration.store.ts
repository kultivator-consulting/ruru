import { defineStore } from 'pinia'
import { ref } from 'vue'

import { api } from '@/api/client'

export type Corner = [number, number]
/** Four board corners, normalized 0..1, order TL, TR, BR, BL. */
export type Corners = [Corner, Corner, Corner, Corner]

export interface Calibration {
  corners: Corners | null
  aspect: [number, number]
}

export const useCalibrationStore = defineStore('calibration', () => {
  const corners = ref<Corners | null>(null)
  const aspect = ref<[number, number]>([3, 2])
  const error = ref<string | null>(null)

  async function load(): Promise<void> {
    try {
      const cal = await api.get<Calibration>('/api/calibration')
      corners.value = cal.corners
      aspect.value = cal.aspect
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  }

  /** Persist corners (null clears calibration) with the current aspect. */
  async function save(next: Corners | null): Promise<void> {
    await api.put('/api/calibration', { corners: next, aspect: aspect.value })
    corners.value = next
  }

  /** POST the given corners; resolves to an object URL for the dewarped JPEG preview. */
  async function dewarpPreview(next: Corners): Promise<string> {
    const res = await fetch('/api/dewarp-preview', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ corners: next, aspect: aspect.value }),
      credentials: 'same-origin',
    })
    if (!res.ok) throw new Error(`dewarp preview failed (${res.status})`)
    return URL.createObjectURL(await res.blob())
  }

  return { corners, aspect, error, load, save, dewarpPreview }
})

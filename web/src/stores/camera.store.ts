import { defineStore } from 'pinia'
import { ref } from 'vue'

import { api } from '@/api/client'

/** Mirrors ruru-client's `ControlDesc`. */
export interface Control {
  name: string
  value: number
  min: number
  max: number
  step: number
  default: number
}

/** Mirrors ruru-client's `SourceStatus`. */
export interface Status {
  enabled: boolean
  open: boolean
  pinned: boolean
  subscribers: number
  last_seq: number
  fps: number
  idle_grace_secs: number
  camera_backend: string
  last_error: string | null
  control_warnings: string[]
}

export const useCameraStore = defineStore('camera', () => {
  const status = ref<Status | null>(null)
  const controls = ref<Control[]>([])
  const error = ref<string | null>(null)

  async function refresh(): Promise<void> {
    try {
      status.value = await api.get<Status>('/api/status')
      controls.value = await api.get<Control[]>('/api/controls')
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  }

  /** Status only — safe to poll without clobbering a slider the user is dragging. */
  async function refreshStatus(): Promise<void> {
    try {
      status.value = await api.get<Status>('/api/status')
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
    }
  }

  async function setControl(name: string, value: number): Promise<void> {
    // The source snaps to the step grid and gates inactive controls, so trust the value it
    // returns rather than what we sent. Mutate by index for reliable reactivity.
    const updated = await api.put<Control>(`/api/controls/${name}`, { value })
    const i = controls.value.findIndex((c) => c.name === name)
    if (i >= 0) controls.value[i] = updated
  }

  async function setEnabled(enabled: boolean): Promise<void> {
    status.value = await api.post<Status>('/api/enable', { enabled })
  }

  return { status, controls, error, refresh, refreshStatus, setControl, setEnabled }
})

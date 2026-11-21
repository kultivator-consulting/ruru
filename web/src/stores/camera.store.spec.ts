import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('@/api/client', () => ({
  api: { get: vi.fn(), post: vi.fn(), put: vi.fn() },
}))

import { api } from '@/api/client'

import { useCameraStore } from './camera.store'

describe('camera store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
  })

  it('setControl adopts the value the source actually applied', async () => {
    const store = useCameraStore()
    store.controls = [{ name: 'focus_absolute', value: 0, min: 0, max: 255, step: 1, default: 0 }]

    // The source gates/snaps, so it may return something other than what we sent.
    vi.mocked(api.put).mockResolvedValue({
      name: 'focus_absolute',
      value: 40,
      min: 0,
      max: 255,
      step: 1,
      default: 0,
    })

    await store.setControl('focus_absolute', 40)
    expect(store.controls[0].value).toBe(40)
    expect(api.put).toHaveBeenCalledWith('/api/controls/focus_absolute', { value: 40 })
  })

  it('refresh records the error message when the source is unreachable', async () => {
    const store = useCameraStore()
    vi.mocked(api.get).mockRejectedValue(new Error('bad gateway'))
    await store.refresh()
    expect(store.error).toBe('bad gateway')
  })
})

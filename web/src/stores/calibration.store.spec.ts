import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('@/api/client', () => ({
  api: { get: vi.fn(), put: vi.fn() },
}))

import { api } from '@/api/client'

import { type Corners, useCalibrationStore } from './calibration.store'

const CORNERS: Corners = [
  [0.1, 0.1],
  [0.9, 0.1],
  [0.9, 0.9],
  [0.1, 0.9],
]

describe('calibration store', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
  })

  it('load pulls corners and aspect', async () => {
    vi.mocked(api.get).mockResolvedValue({ corners: CORNERS, aspect: [16, 10] })
    const store = useCalibrationStore()
    await store.load()
    expect(store.aspect).toEqual([16, 10])
    expect(store.corners?.[1]).toEqual([0.9, 0.1])
  })

  it('save PUTs the corners with the current aspect and updates state', async () => {
    const store = useCalibrationStore()
    store.aspect = [4, 3]
    await store.save(CORNERS)
    expect(api.put).toHaveBeenCalledWith('/api/calibration', {
      corners: CORNERS,
      aspect: [4, 3],
    })
    expect(store.corners).toEqual(CORNERS)
  })

  it('save(null) clears calibration', async () => {
    const store = useCalibrationStore()
    await store.save(null)
    expect(api.put).toHaveBeenCalledWith('/api/calibration', {
      corners: null,
      aspect: [3, 2],
    })
    expect(store.corners).toBeNull()
  })
})

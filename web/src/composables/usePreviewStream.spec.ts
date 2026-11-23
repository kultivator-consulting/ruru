import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { defineComponent, ref } from 'vue'

import { PREVIEW_MAX_MS, usePreviewStream } from './usePreviewStream'

function setHidden(value: boolean): void {
  Object.defineProperty(document, 'hidden', { configurable: true, get: () => value })
  document.dispatchEvent(new Event('visibilitychange'))
}

function harness(enabled = ref(true)) {
  let api!: ReturnType<typeof usePreviewStream>
  const wrapper = mount(
    defineComponent({
      setup() {
        api = usePreviewStream('/api/preview.mjpeg', enabled)
        return () => null
      },
    }),
  )
  return { api, wrapper }
}

describe('usePreviewStream', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    setHidden(false)
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('streams while visible and enabled', () => {
    const { api } = harness()
    expect(api.src.value).toMatch(/^\/api\/preview\.mjpeg\?k=\d+$/)
  })

  it('drops the stream while the tab is hidden and reconnects fresh on return', () => {
    const { api } = harness()
    const before = api.src.value
    setHidden(true)
    expect(api.src.value).toBeNull()
    setHidden(false)
    expect(api.src.value).not.toBeNull()
    expect(api.src.value).not.toBe(before)
  })

  it('pauses after the duration cap until resumed', () => {
    const { api } = harness()
    vi.advanceTimersByTime(PREVIEW_MAX_MS)
    expect(api.paused.value).toBe(true)
    expect(api.src.value).toBeNull()
    api.restart()
    expect(api.paused.value).toBe(false)
    expect(api.src.value).not.toBeNull()
  })

  it('does not stream when the camera is disabled', () => {
    const { api } = harness(ref(false))
    expect(api.src.value).toBeNull()
  })

  it('stops listening once unmounted', () => {
    const { api, wrapper } = harness()
    wrapper.unmount()
    vi.advanceTimersByTime(PREVIEW_MAX_MS)
    expect(api.paused.value).toBe(false)
  })
})

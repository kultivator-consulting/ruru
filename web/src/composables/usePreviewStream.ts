import { computed, onBeforeUnmount, onMounted, ref, type Ref } from 'vue'

/** Matches the server's cap (`PREVIEW_MAX_DURATION` in admin.rs): one stream runs ≤ 10 min. */
export const PREVIEW_MAX_MS = 10 * 60 * 1000

/**
 * The live MJPEG preview URL, or `null` when it shouldn't be streaming.
 *
 * Holding the stream holds the camera open, so it is dropped whenever nobody can be
 * looking: the tab is hidden (switched away, minimised, phone locked), or it has run for
 * PREVIEW_MAX_MS, after which it pauses until the user clicks Resume. Dropping the
 * `<img>` src aborts the request, which detaches us from `ruru-source`.
 */
export function usePreviewStream(base: string, enabled: Ref<boolean> = ref(true)) {
  const hidden = ref(typeof document !== 'undefined' && document.hidden)
  const paused = ref(false)
  const key = ref(0)
  let timer: number | undefined

  function armTimer(): void {
    window.clearTimeout(timer)
    timer = window.setTimeout(() => {
      paused.value = true
    }, PREVIEW_MAX_MS)
  }

  /** Start a fresh stream (also used after re-enabling the camera). */
  function restart(): void {
    paused.value = false
    key.value++
    armTimer()
  }

  function onVisibility(): void {
    hidden.value = document.hidden
    // Coming back: reconnect with a fresh request rather than a stale cached one.
    if (!document.hidden && !paused.value) restart()
  }

  onMounted(() => {
    document.addEventListener('visibilitychange', onVisibility)
    armTimer()
  })
  onBeforeUnmount(() => {
    document.removeEventListener('visibilitychange', onVisibility)
    window.clearTimeout(timer)
  })

  const src = computed(() =>
    enabled.value && !hidden.value && !paused.value ? `${base}?k=${key.value}` : null,
  )

  return { src, paused, restart }
}

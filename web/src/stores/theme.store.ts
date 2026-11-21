import { defineStore } from 'pinia'
import { ref } from 'vue'

export type ThemeMode = 'light' | 'dark' | 'system'

const STORAGE_KEY = 'ruru:theme:mode'

function systemPrefersDark(): boolean {
  if (typeof window === 'undefined' || !window.matchMedia) return false
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

function applyClass(resolved: 'light' | 'dark') {
  if (typeof document === 'undefined') return
  document.documentElement.classList.toggle('dark', resolved === 'dark')
}

// Module-scoped so we can detach the listener if `init` is called more
// than once (only happens in tests).
let mediaQuery: MediaQueryList | null = null
let mediaListener: ((e: MediaQueryListEvent) => void) | null = null

export const useThemeStore = defineStore('theme', () => {
  const mode = ref<ThemeMode>('system')
  const resolved = ref<'light' | 'dark'>('light')

  function resolve(m: ThemeMode): 'light' | 'dark' {
    if (m === 'system') return systemPrefersDark() ? 'dark' : 'light'
    return m
  }

  function apply(next: ThemeMode) {
    mode.value = next
    resolved.value = resolve(next)
    applyClass(resolved.value)
  }

  function setMode(next: ThemeMode) {
    apply(next)
    try {
      localStorage.setItem(STORAGE_KEY, next)
    } catch {
      // private mode / quota — ignore.
    }
  }

  function cycle() {
    const order: ThemeMode[] = ['light', 'dark', 'system']
    const idx = order.indexOf(mode.value)
    setMode(order[(idx + 1) % order.length])
  }

  function init() {
    let saved: ThemeMode = 'system'
    try {
      const v = localStorage.getItem(STORAGE_KEY) as ThemeMode | null
      if (v === 'light' || v === 'dark' || v === 'system') saved = v
    } catch {
      // ignore
    }
    apply(saved)

    if (typeof window !== 'undefined' && window.matchMedia) {
      if (mediaQuery && mediaListener) {
        mediaQuery.removeEventListener('change', mediaListener)
      }
      mediaQuery = window.matchMedia('(prefers-color-scheme: dark)')
      mediaListener = (e: MediaQueryListEvent) => {
        if (mode.value === 'system') {
          resolved.value = e.matches ? 'dark' : 'light'
          applyClass(resolved.value)
        }
      }
      mediaQuery.addEventListener('change', mediaListener)
    }
  }

  return { mode, resolved, init, setMode, cycle }
})

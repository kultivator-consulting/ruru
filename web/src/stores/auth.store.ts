import { defineStore } from 'pinia'
import { ref } from 'vue'

import { api } from '@/api/client'

export const useAuthStore = defineStore('auth', () => {
  const authenticated = ref(false)

  /** Resolve the current session before the router guard runs, so it has truth. */
  async function fetchMe(): Promise<void> {
    try {
      await api.get('/api/me')
      authenticated.value = true
    } catch {
      authenticated.value = false
    }
  }

  async function login(password: string): Promise<void> {
    await api.post('/api/login', { password })
    authenticated.value = true
  }

  async function logout(): Promise<void> {
    try {
      await api.post('/api/logout')
    } finally {
      authenticated.value = false
    }
  }

  return { authenticated, fetchMe, login, logout }
})

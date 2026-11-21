import { createPinia, setActivePinia } from 'pinia'
import type { RouteLocationNormalized } from 'vue-router'
import { beforeEach, describe, expect, it } from 'vitest'

import { useAuthStore } from '@/stores/auth.store'

import { authGuard } from './index'

function to(name: string, meta: Record<string, unknown> = {}): RouteLocationNormalized {
  return { name, meta } as unknown as RouteLocationNormalized
}

describe('authGuard', () => {
  beforeEach(() => setActivePinia(createPinia()))

  it('sends anonymous users to login', () => {
    expect(authGuard(to('controls'))).toEqual({ name: 'login' })
  })

  it('lets authenticated users through', () => {
    useAuthStore().authenticated = true
    expect(authGuard(to('controls'))).toBe(true)
  })

  it('bounces authenticated users off the public login page', () => {
    useAuthStore().authenticated = true
    expect(authGuard(to('login', { public: true }))).toEqual({ name: 'controls' })
  })

  it('allows anonymous access to public pages', () => {
    expect(authGuard(to('login', { public: true }))).toBe(true)
  })
})

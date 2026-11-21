import { createRouter, createWebHistory } from 'vue-router'
import type { RouteLocationNormalized, RouteLocationRaw } from 'vue-router'

import { useAuthStore } from '@/stores/auth.store'

const routes = [
  {
    path: '/login',
    name: 'login',
    component: () => import('@/views/LoginView.vue'),
    meta: { public: true },
  },
  { path: '/', name: 'controls', component: () => import('@/views/ControlsView.vue') },
  {
    path: '/calibration',
    name: 'calibration',
    component: () => import('@/views/CalibrationView.vue'),
  },
  { path: '/keys', name: 'keys', component: () => import('@/views/KeysView.vue') },
  { path: '/:pathMatch(.*)*', redirect: '/' },
]

/** Unauthenticated visitors go to /login; an authenticated one is bounced off /login. */
export function authGuard(to: RouteLocationNormalized): RouteLocationRaw | boolean {
  const auth = useAuthStore()
  if (to.meta.public) {
    return auth.authenticated ? { name: 'controls' } : true
  }
  return auth.authenticated ? true : { name: 'login' }
}

const router = createRouter({
  history: createWebHistory(),
  routes,
})

router.beforeEach(authGuard)

export default router

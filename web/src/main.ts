import { createApp } from 'vue'
import { createPinia } from 'pinia'

import App from './App.vue'
import { setUnauthorizedHandler } from './api/client'
import router from './router'
import { useAuthStore } from './stores/auth.store'
import { useThemeStore } from './stores/theme.store'
import './styles/main.css'

async function bootstrap(): Promise<void> {
  const app = createApp(App)
  app.use(createPinia())

  // Init before mount to avoid a flash of the wrong theme.
  useThemeStore().init()

  const auth = useAuthStore()

  // Any 401 clears auth and returns to login, from anywhere in the app.
  setUnauthorizedHandler(() => {
    auth.authenticated = false
    void router.replace({ name: 'login' })
  })

  // Resolve the session before mounting so the guard sees the truth on first paint.
  await auth.fetchMe()

  app.use(router)
  app.mount('#app')
}

void bootstrap()

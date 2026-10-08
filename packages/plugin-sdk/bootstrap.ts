import type { Component } from 'vue'
import { createApp, h, watch } from 'vue'
import { createPinia } from 'pinia'
import { createMemoryHistory, createRouter, RouterView } from 'vue-router'
import { BaseToast } from '../../frontend/src/components/base/BaseToast'
import BaseScrollbar from '../../frontend/src/components/base/BaseScrollbar.vue'
import { loading } from '../../frontend/src/directives/loading'
import { context, host } from './ui'
import { installHostNavigation } from './navigation'
import { useThemeStore } from '../../frontend/src/stores/modules/theme'
import type { ThemeMode, ThemeColorId, ThemeCustomization } from '../../frontend/src/theme'
import { useAuthStore } from '../../frontend/src/stores/modules/auth'
import '../../frontend/src/styles/index.css'
import './layout.css'

export async function mount(pages: Record<string, { component: Component, route: string, name: string }>) {
  const ctx = context()
  const routes = Object.values(pages).map(page => ({ path: page.route, name: page.name, component: page.component, props: { active: true } }))
  const router = createRouter({ history: createMemoryHistory(), routes })
  installHostNavigation(router, ctx.route || routes[0]!.path, path => host('navigate', { path }))
  const app = createApp({ render: () => h('div', { class: 'plugin-viewport' }, [
    h(BaseScrollbar, {}, { default: () => h('div', { class: ['plugin-root', ctx.presentation === 'popup' ? 'plugin-popup' : 'plugin-page', ctx.settingsShell ? 'plugin-settings' : ''] }, [
      h(RouterView, {}, { default: ({ Component }: { Component: Component }) => Component ? h(Component, { class: 'min-h-0 flex-1' }) : null }),
    ]) }), h(BaseToast),
  ]) })
  const pinia = createPinia()
  app.use(pinia)
  const themeStore = useThemeStore(pinia)
  type Configuration = { mode: ThemeMode, color: ThemeColorId, customColor: string, customization: ThemeCustomization }
  let syncing = false
  function syncTheme() {
    syncing = true
    const configuration = (context() as unknown as { themeConfiguration?: Configuration }).themeConfiguration
    if (configuration) themeStore.setThemeConfiguration(configuration)
    syncing = false
  }
  syncTheme()
  themeStore.initializeTheme()
  window.addEventListener('plugin-context', syncTheme)
  watch(() => JSON.stringify([themeStore.themeMode,themeStore.themeColor,themeStore.customThemeColor,themeStore.themeCustomization]), () => {
    if (!syncing) void host('theme', { mode: themeStore.themeMode, color: themeStore.themeColor, customColor: themeStore.customThemeColor, customization: themeStore.themeCustomization }).catch(() => {})
  }, { flush: 'sync' })
  await useAuthStore(pinia).checkAuth()
  app.use(router)
  app.directive('loading', loading)
  for (const [name, value] of Object.entries(ctx.theme ?? {})) {
    if (name.startsWith('--cp-'))
      document.documentElement.style.setProperty(name, value)
  }
  await router.push(ctx.route || routes[0]!.path)
  await router.isReady()
  app.mount('#app')
  await host('ready', {})
}

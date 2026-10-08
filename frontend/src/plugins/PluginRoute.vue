<script setup lang="ts">
import type { PlatformPage, PlatformPlugin } from './platform'
import type { SettingsActions } from './settingsActions'
import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { useRoute } from 'vue-router'
import { retainPages } from './pageCache'
import { platformPlugins } from './platform'
import PluginFrame from './PluginFrame.vue'
import SettingsHostHeader from './SettingsHostHeader.vue'

const route = useRoute()
const available = computed(() => platformPlugins.value.flatMap(plugin => plugin.pages
  .filter(page => page.route)
  .map(page => ({ key: `${plugin.id}/${page.id}`, version: plugin.contentVersion, plugin, page, touched: 0 }))))
const selected = computed(() => available.value.find(item => item.page.route === route.path))
const cached = shallowRef<{ key: string, version: string | null, plugin: PlatformPlugin, page: PlatformPage, touched: number }[]>([])
const displayedKey = shallowRef<string>()
const activeEntry = computed(() => cached.value.find(item => item.key === (displayedKey.value || selected.value?.key)))
const showSettingsHeader = computed(() => activeEntry.value?.page.layout === 'settings')
const actionsByPage = shallowRef<Record<string, SettingsActions>>({})
const currentActions = computed(() => activeEntry.value?.plugin.id === 'gateway-settings' ? actionsByPage.value[activeEntry.value.key] : undefined)
const frames = new Map<string, { sendSettingsAction: (action: 'save' | 'reset') => void }>()
function trackFrame(key: string, instance: unknown) {
  if (instance)
    frames.set(key, instance as { sendSettingsAction: (action: 'save' | 'reset') => void })
  else frames.delete(key)
}
function updateSettingsActions(key: string, value: SettingsActions) {
  actionsByPage.value = { ...actionsByPage.value, [key]: value }
}
function settingsAction(action: 'save' | 'reset') {
  if (activeEntry.value)
    frames.get(activeEntry.value.key)?.sendSettingsAction(action)
}
let visit = 0
let preloadTimer: ReturnType<typeof setTimeout> | undefined
const settled = new Set<string>()
const commonRoutes = ['/', '/accounts', '/groups', '/keys', '/usage', '/proxies', '/settings', '/settings/upstream', '/settings/pricing', '/settings/access', '/settings/backup', '/theme']
function schedulePreload() {
  if (preloadTimer !== undefined)
    return
  preloadTimer = setTimeout(() => {
    preloadTimer = undefined
    if (document.hidden)
      return
    const pending = cached.value.filter(item => !settled.has(`${item.key}/${item.version}`)).length
    const room = Math.max(0, 3 - pending)
    const ordered = [...available.value].sort((a, b) => {
      const priority = (path: string) => (route.path.startsWith('/settings') && path.startsWith('/settings') ? -100 : 0) + (commonRoutes.indexOf(path) + 1 || 99)
      return priority(a.page.route!) - priority(b.page.route!)
    })
    const next = ordered.filter(item => item.page.runtime !== 'vue-component' && !item.plugin.lazyLoad && !cached.value.some(page => page.key === item.key)).slice(0, room)
    if (next.length)
      cached.value = [...cached.value, ...next]
  }, 0)
}
function pageSettled(key: string, version: string | null) {
  settled.add(`${key}/${version}`)
  if (key === selected.value?.key)
    displayedKey.value = key
  schedulePreload()
}
function visibilityChanged() {
  if (!document.hidden)
    schedulePreload()
}
document.addEventListener('visibilitychange', visibilityChanged)
onScopeDispose(() => {
  clearTimeout(preloadTimer)
  document.removeEventListener('visibilitychange', visibilityChanged)
})
watch([available, () => route.path], () => {
  if (!available.value.length) {
    clearTimeout(preloadTimer)
    preloadTimer = undefined
    settled.clear()
    displayedKey.value = undefined
    actionsByPage.value = {}
  }
  cached.value = retainPages(cached.value, selected.value && { ...selected.value, touched: ++visit }, new Map(available.value.map(item => [item.key, item.version])))
  const target = cached.value.find(item => item.key === selected.value?.key)
  if (target && settled.has(`${target.key}/${target.version}`))
    displayedKey.value = target.key
  else if (!cached.value.some(item => item.key === displayedKey.value))
    displayedKey.value = undefined
  if (available.value.length)
    schedulePreload()
}, { immediate: true, flush: 'sync' })
</script>

<template>
  <div class="relative flex h-dvh flex-col" :aria-busy="!!selected && displayedKey !== selected.key">
    <SettingsHostHeader v-show="showSettingsHeader" :actions="currentActions" @action="settingsAction" />
    <div class="relative min-h-0 flex-1">
      <div v-if="displayedKey && selected && displayedKey !== selected.key" role="status" aria-label="正在准备页面" class="absolute inset-x-0 top-0 z-10 h-0.5 bg-cp-primary" />
      <PluginFrame v-for="item in cached" v-show="item.key === (displayedKey || selected?.key)" :key="item.key" :ref="instance => trackFrame(item.key, instance)" class="absolute inset-0 h-full" :active="item.key === (displayedKey || selected?.key)" :plugin="item.plugin" :page="item.page" @ready="pageSettled(item.key, item.version)" @failed="pageSettled(item.key, item.version)" @settings-actions="value => updateSettingsActions(item.key, value)" />
      <div v-if="!selected" class="p-6 text-cp-text-secondary">
        模块未安装、已停用或当前身份不可用。管理员可在插件管理中恢复。
      </div>
    </div>
  </div>
</template>

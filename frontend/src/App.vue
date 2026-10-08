<script setup lang="ts">
import { onScopeDispose, watch } from 'vue'
import { RouterView } from 'vue-router'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { BaseToast } from '@/components/base/BaseToast'
import { pluginPopup, refreshPlatform, resetPlatform } from '@/plugins/platform'
import PluginFrame from '@/plugins/PluginFrame.vue'
import { useAuthStore } from '@/stores/modules/auth'

const auth = useAuthStore()
let timer: ReturnType<typeof setTimeout> | undefined
let stopped = false
let refreshGeneration = 0
async function refresh() {
  const current = ++refreshGeneration
  clearTimeout(timer)
  if (!auth.isAuthenticated || stopped)
    return
  try {
    await refreshPlatform()
  }
  catch { /* 登录恢复期间保留错误边界。 */ }
  if (current === refreshGeneration && auth.isAuthenticated && !stopped)
    timer = setTimeout(refresh, 15000)
}
watch(() => auth.session, () => {
  resetPlatform()
  pluginPopup.value = undefined
  void refresh()
}, { immediate: true })
function focus() {
  void refresh()
}
window.addEventListener('focus', focus)
onScopeDispose(() => {
  stopped = true
  clearTimeout(timer)
  window.removeEventListener('focus', focus)
})
</script>

<template>
  <RouterView />
  <BaseToast />
  <BaseModal :model-value="!!pluginPopup" :title="pluginPopup?.page.title || '插件'" :size="pluginPopup?.plugin.id === 'account-diagnostics' ? 'xl' : 'lg'" :scrollable="!['account-diagnostics', 'client-access'].includes(pluginPopup?.plugin.id ?? '')" @update:model-value="value => { if (!value) pluginPopup = undefined }">
    <PluginFrame v-if="pluginPopup" style="height: min(600px, calc(100dvh - 12rem))" :scrollable="!['account-diagnostics', 'client-access'].includes(pluginPopup.plugin.id)" :plugin="pluginPopup.plugin" :page="pluginPopup.page" :context="pluginPopup.context" @close="pluginPopup = undefined" />
  </BaseModal>
</template>

<script setup lang="ts">
import type { PluginAction, PluginStatus } from '@/api/modules/system'
import { computed, onScopeDispose, ref, watch } from 'vue'
import { getPlugins, performPluginAction } from '@/api/modules/system'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { invalidatePlugin, refreshPlugins } from '@/plugins/catalog'
import PluginSlot from '@/plugins/PluginSlot.vue'

const props = defineProps<{ active: boolean }>()
const plugins = ref<PluginStatus[]>([])
const loading = ref(false)
const submitting = ref(false)
const error = ref('')
const loaded = ref(false)
const confirmUninstall = ref(false)
const selectedId = ref('')
const busy = computed(() => submitting.value || plugins.value.some(p => p.operation.status === 'running'))
let timer: ReturnType<typeof setTimeout> | undefined
let disposed = false

async function refresh() {
  if (loading.value)
    return
  loading.value = true
  try {
    plugins.value = await getPlugins()
    await refreshPlugins()
    loaded.value = true
    error.value = ''
  }
  catch {
    error.value = '无法读取插件状态，请稍后刷新或检查插件管理服务。'
  }
  finally {
    loading.value = false
    clearTimeout(timer)
    if (!disposed && props.active)
      timer = setTimeout(refresh, busy.value ? 1500 : 10000)
  }
}

async function act(id: string, action: PluginAction) {
  submitting.value = true
  error.value = ''
  try {
    await performPluginAction(id, action)
    invalidatePlugin(id)
    confirmUninstall.value = false
    await refresh()
  }
  catch {
    error.value = '操作未受理：请确认没有其他插件或系统更新正在执行，然后刷新重试。'
  }
  finally {
    submitting.value = false
  }
}

watch(() => props.active, (active) => {
  clearTimeout(timer)
  if (active)
    void refresh()
}, { immediate: true })
onScopeDispose(() => {
  disposed = true
  clearTimeout(timer)
})
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <p class="text-cp-sm text-cp-text-secondary">
        插件独立安装和更新。禁用会等待现有请求结束，普通 API 转发继续运行。
      </p>
      <BaseButton variant="secondary" :loading="loading" @click="refresh">
        刷新状态
      </BaseButton>
    </div>
    <p v-if="error" role="alert" class="text-cp-sm text-cp-error-text">
      {{ error }}
    </p>
    <p v-if="loaded && !plugins.length" class="text-cp-sm text-cp-text-secondary">
      此实例尚未配置独立插件管理服务。
    </p>
    <BaseCard v-for="plugin in plugins" :key="plugin.id" :title="plugin.name" :description="plugin.description">
      <template #body>
        <dl class="grid grid-cols-2 gap-4 text-cp-sm md:grid-cols-4">
          <div>
            <dt class="text-cp-text-secondary">
              状态
            </dt><dd class="mt-1 font-medium">
              {{ plugin.operation.status === 'running' ? '处理中' : !plugin.installed ? '未安装' : plugin.enabled ? '已启用 · 按需启动' : '已禁用' }}
            </dd>
          </div>
          <div>
            <dt class="text-cp-text-secondary">
              已安装版本
            </dt><dd class="mt-1">
              {{ plugin.version || '—' }}
            </dd>
          </div>
          <div>
            <dt class="text-cp-text-secondary">
              可安装版本
            </dt><dd class="mt-1">
              {{ plugin.availableVersion }}
            </dd>
          </div>
          <div>
            <dt class="text-cp-text-secondary">
              接口协议
            </dt><dd class="mt-1">
              v{{ plugin.protocol }}
            </dd>
          </div>
        </dl>
        <p v-if="plugin.operation.message" role="status" class="mt-4 text-cp-sm" :class="plugin.operation.status === 'failed' ? 'text-cp-error-text' : 'text-cp-text-secondary'">
          {{ plugin.operation.message }}
        </p>
        <div class="mt-5 flex flex-wrap gap-2">
          <BaseButton v-if="!plugin.installed" :disabled="busy" @click="act(plugin.id, 'install')">
            安装并启用
          </BaseButton>
          <BaseButton v-if="plugin.installed && !plugin.enabled" :disabled="busy" @click="act(plugin.id, 'enable')">
            启用
          </BaseButton>
          <BaseButton v-if="plugin.installed && plugin.enabled" variant="secondary" :disabled="busy" @click="act(plugin.id, 'disable')">
            禁用
          </BaseButton>
          <BaseButton v-if="plugin.installed" variant="secondary" :disabled="busy" @click="act(plugin.id, 'update')">
            {{ plugin.version !== plugin.availableVersion ? '更新插件' : '重新安装' }}
          </BaseButton>
          <BaseButton v-if="plugin.installed" variant="secondary" :disabled="busy" @click="selectedId = plugin.id; confirmUninstall = true">
            卸载
          </BaseButton>
        </div>
        <PluginSlot v-if="plugin.enabled" placement="settings" class="mt-3" :plugin-id="plugin.id" />
        <p class="mt-4 text-cp-xs text-cp-text-secondary">
          禁用后隐藏功能入口并暂停插件服务，保留配置和使用记录。
        </p>
      </template>
    </BaseCard>
    <BaseModal v-model="confirmUninstall" :title="`卸载 ${plugins.find(p => p.id === selectedId)?.name || '插件'}`" tone="warning" size="sm" :dismissible="!submitting" description="等待现有插件请求结束后，移除插件运行文件并关闭开机启动。保留密钥和使用记录，以后可重新安装。">
      <template #footer>
        <BaseButton variant="secondary" :disabled="submitting" @click="confirmUninstall = false">
          取消
        </BaseButton>
        <BaseButton :loading="submitting" @click="act(selectedId, 'uninstall')">
          确认卸载
        </BaseButton>
      </template>
    </BaseModal>
  </div>
</template>

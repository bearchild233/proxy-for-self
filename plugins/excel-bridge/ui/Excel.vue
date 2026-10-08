<script setup lang="ts">
import { usePluginPolling } from '@sdk/polling'
import { call, context } from '@sdk/ui'
import { onMounted, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'

const status = ref<{ installed: boolean, enabled: boolean, version?: string, operation?: { status: string, message: string } }>()
const error = ref('')
let loading = false
async function load(silent = false) {
  if (loading)
    return
  loading = true
  try {
    status.value = await call('excel.status', {})
    error.value = ''
  }
  catch (cause) {
    if (!silent)
      error.value = cause instanceof Error ? cause.message : 'Worker 状态不可用'
  }
  finally { loading = false }
}
usePluginPolling(() => load(true), ref(30_000))
onMounted(() => load())
</script>

<template>
  <div class="space-y-5 text-cp-text">
    <h2 class="text-cp-lg font-emphasis">
      Excel Bridge
    </h2>
    <p class="text-cp-sm text-cp-text-secondary">
      Excel 请求由独立 Worker 执行。为密钥启用 Excel 能力后，可在“导入配置”中选择 Excel 模型。
    </p>
    <div v-if="status" class="rounded-cp bg-cp-bg-layout p-4 text-cp-sm">
      <p>Worker：{{ status.installed ? status.enabled ? '运行中' : '已停止' : '未安装' }} {{ status.version || '' }}</p><p class="mt-2">
        {{ status.operation?.message }}
      </p>
    </div>
    <p v-if="error" role="alert" class="text-cp-error-text">
      {{ error }}
    </p>
    <p v-if="context().role === 'admin'" class="text-cp-sm text-cp-text-secondary">
      在插件管理中统一安装、更新或停用。更新先关闭新请求，等待正在执行的任务完成；失败会恢复旧版本。
    </p>
    <BaseButton @click="load()">
      刷新状态
    </BaseButton>
  </div>
</template>

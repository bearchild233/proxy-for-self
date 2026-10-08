<script setup lang="ts">
import type { ApiKey } from '@/api'
import { excelAvailable } from '@/plugins/catalog'

defineProps<{
  apiKey: ApiKey
}>()
</script>

<template>
  <div class="grid w-full justify-items-center gap-1.5">
    <div v-if="!apiKey.boundAccountId && !apiKey.routing" class="flex items-center justify-center gap-2">
      <span class="inline-flex h-6 items-center rounded-lg bg-cp-warning-container px-2 text-cp-xs font-bold text-cp-warning-on-container">
        未绑定 访问已拒绝
      </span>
    </div>
    <template v-else>
      <span class="text-cp-xs text-cp-text-secondary">{{ apiKey.routing?.mode === 'groups' ? `${apiKey.routing.groupIds.length} 个账号组` : apiKey.routing?.mode === 'accounts' ? `${apiKey.routing.accountIds.length} 个指定账号` : '固定账号' }}</span>
      <span class="text-cp-xs text-cp-text">{{ apiKey.excelBridgeEnabled ? (excelAvailable ? 'Excel Bridge' : '服务已停用') : 'Codex Native' }}</span>
    </template>
  </div>
</template>

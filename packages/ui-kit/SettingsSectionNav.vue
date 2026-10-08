<script setup lang="ts">
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { activePlugins } from '@sdk/catalog'
import { context } from '@sdk/ui'
const settingsShell = context().settingsShell === true

const route = useRoute()
const router = useRouter()
const options = computed(() => [
  { label: '网关调度', value: '/settings' },
  { label: '上游配置', value: '/settings/upstream' },
  { label: '模型价格', value: '/settings/pricing' },
  { label: '安全与访问', value: '/settings/access' },
  { label: '备份管理', value: '/settings/backup' },
].filter(item => activePlugins.value.some(plugin => plugin.pages.some(page => page.route === item.value))))
function navigate(path: string) { void router.push(path) }
</script>

<template>
  <header v-if="!settingsShell" class="mb-5 shrink-0">
    <BasePageHeader title="系统设置" description="管理网关调度、上游配置、模型定价、安全访问与数据备份" />
    <div class="mt-4 flex min-h-cp-control flex-wrap items-center justify-between gap-3">
      <BaseSegmented :model-value="route.path" label="设置分区" class="hidden! bg-(--cp-input-bg)! sm:inline-grid!" :options="options" @update:model-value="navigate" />
      <BaseSelect :model-value="route.path" aria-label="设置分区" class="w-full sm:hidden" :options="options" @update:model-value="navigate" />
      <slot />
    </div>
  </header>
</template>

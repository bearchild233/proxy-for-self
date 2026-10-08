<script setup lang="ts">
import type { SettingsActions } from './settingsActions'
import { Save, Undo2 } from '@lucide/vue'
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { platformPlugins } from './platform'

defineProps<{ actions?: SettingsActions }>()
const emit = defineEmits<{ action: [value: 'save' | 'reset'] }>()
const route = useRoute()
const router = useRouter()
const options = computed(() => [
  { label: '网关调度', value: '/settings' },
  { label: '上游配置', value: '/settings/upstream' },
  { label: '模型价格', value: '/settings/pricing' },
  { label: '安全与访问', value: '/settings/access' },
  { label: '备份管理', value: '/settings/backup' },
].filter(item => platformPlugins.value.some(plugin => plugin.pages.some(page => page.route === item.value))))
function navigate(path: string) {
  void router.push(path)
}
</script>

<template>
  <header data-settings-host-header class="shrink-0 px-4 pb-5 pt-4 min-[961px]:px-6 min-[961px]:pt-6">
    <BasePageHeader title="系统设置" description="管理网关调度、上游配置、模型定价、安全访问与数据备份" />
    <div class="mt-4 flex min-h-cp-control flex-wrap items-center justify-between gap-3">
      <BaseSegmented :model-value="route.path" label="设置分区" class="hidden! bg-(--cp-input-bg)! sm:inline-grid!" :options="options" @update:model-value="navigate" />
      <BaseSelect :model-value="route.path" aria-label="设置分区" class="w-full sm:hidden" :options="options" @update:model-value="navigate" />
      <div v-if="actions" class="ml-auto flex items-center justify-end gap-2">
        <span v-if="actions.changed" class="mr-1 size-1.5 shrink-0 rounded-full bg-cp-warning" aria-hidden="true" />
        <BaseIconButton v-if="actions.changed" label="撤销全部基础设置更改" variant="filled" :disabled="actions.resetDisabled" @click="emit('action', 'reset')">
          <Undo2 class="size-4" />
        </BaseIconButton>
        <BaseButton variant="primary" :loading="actions.saving" :disabled="actions.saveDisabled" @click="emit('action', 'save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ actions.saving ? '保存中...' : '保存基础设置' }}
        </BaseButton>
      </div>
    </div>
  </header>
</template>

<script setup lang="ts">
import { ChevronDown, Flame, Settings2 } from '@lucide/vue'
import { host } from '@sdk/ui'
import { ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BasePopover from '@/components/base/BasePopover.vue'

defineProps<{ selectedAccountIds: string[], archived: boolean, ready: boolean, installed: boolean, submitting: boolean, status: string }>()
const emit = defineEmits<{ run: [ids?: string[]] }>()
const error = ref('')
async function settings() {
  try {
    await host('navigate', { path: '/extensions/account-warmup' })
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '自动预热设置暂不可用' }
}
</script>

<template>
  <div class="flex flex-col items-end gap-1" :title="status">
    <div class="flex items-center gap-1.5">
      <BaseButton variant="secondary" :loading="submitting" :disabled="!ready || archived || submitting || selectedAccountIds.length > 500" @click="emit('run', selectedAccountIds.length ? selectedAccountIds : undefined)">
        <Flame class="size-4" /> {{ selectedAccountIds.length ? `预热选中 (${selectedAccountIds.length})` : '预热全部账号' }}
      </BaseButton>
      <BasePopover placement="bottom-end">
        <template #trigger="{ open }">
          <BaseIconButton label="选择预热范围" :pressed="open" :disabled="!ready || archived || submitting">
            <ChevronDown class="size-4" />
          </BaseIconButton>
        </template>
        <template #default="{ close }">
          <div class="w-52 p-1.5">
            <BaseMenuItem :disabled="!ready || archived || submitting" @click="close(); emit('run')">
              预热全部当前账号
            </BaseMenuItem>
            <BaseMenuItem :disabled="!ready || archived || submitting || !selectedAccountIds.length || selectedAccountIds.length > 500" @click="close(); emit('run', selectedAccountIds)">
              预热选中 ({{ selectedAccountIds.length }})
            </BaseMenuItem>
            <p class="mx-2 my-1 text-cp-xs text-cp-text-secondary">
              全部包含当前账号池，不受列表筛选或页码影响。
            </p>
          </div>
        </template>
      </BasePopover>
      <BaseIconButton label="自动预热设置" :disabled="!installed" @click="settings">
        <Settings2 class="size-4" />
      </BaseIconButton>
    </div>
    <span role="status" class="max-w-96 text-right text-cp-xs" :class="error ? 'text-cp-error-text' : 'text-cp-text-secondary'">{{ error || status }}</span>
  </div>
</template>

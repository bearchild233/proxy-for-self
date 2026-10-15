<script setup lang="ts">
import { MoreHorizontal } from '@lucide/vue'
import { computed } from 'vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseTag from '@/components/base/BaseTag.vue'

// 账号在网关池里的三种用法，对应队列的 enabled 与 degradedUse 两个字段；不存在“停用又允许降智使用”的组合。
// 行内只显示非默认状态的标签，三选一放在“更多”弹窗里。
const props = defineProps<{
  name: string
  enabled: boolean
  degradedUse: boolean
  quotaExhausted: boolean
  saving: boolean
}>()
const emit = defineEmits<{ save: [payload: { enabled: boolean, degradedUse: boolean }] }>()

type Mode = 'pool' | 'direct' | 'off'
const options = [
  { label: '网关池', value: 'pool' },
  { label: '降智直接用', value: 'direct' },
  { label: '停用', value: 'off' },
]
const hints: Record<Mode, string> = {
  pool: '打票，带票走网关池使用。',
  direct: '不打票，按普通账号直接使用；期间留在降智分组，不做恢复检测。',
  off: '不打票，也不会被使用。',
}
const mode = computed<Mode>(() => !props.enabled ? 'off' : props.degradedUse ? 'direct' : 'pool')
function select(value: string) {
  if (value !== mode.value)
    emit('save', { enabled: value !== 'off', degradedUse: value === 'direct' })
}
</script>

<template>
  <div class="flex items-center gap-2">
    <BaseTag v-if="quotaExhausted" size="sm">
      额度已用完
    </BaseTag>
    <BaseTag v-if="mode === 'direct'" type="warning" size="sm">
      降智直接用
    </BaseTag>
    <BaseTag v-else-if="mode === 'off'" size="sm">
      已停用
    </BaseTag>
    <BasePopover placement="bottom-end">
      <template #trigger="{ open }">
        <BaseIconButton variant="ghost" size="sm" label="账号设置" :pressed="open">
          <MoreHorizontal class="size-4" />
        </BaseIconButton>
      </template>
      <div class="grid w-72 gap-2.5 p-3">
        <div class="truncate text-cp-sm font-emphasis text-cp-text">
          {{ name }}
        </div>
        <BaseSegmented :model-value="mode" :label="`${name}的网关池用法`" :options="options" size="sm" :disabled="saving" @update:model-value="select" />
        <p class="text-cp-xs text-cp-text-tertiary">
          {{ hints[mode] }}
        </p>
      </div>
    </BasePopover>
  </div>
</template>

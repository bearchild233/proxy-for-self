<script setup lang="ts">
import type { WarmupOverride, WarmupReasoningEffort } from '@/api/modules/accountWarmup'
import { computed, ref, watch } from 'vue'
import { warmupEffortOptions, warmupRequestError } from '@/api/modules/accountWarmup'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

const props = defineProps<{ count: number, disabled: boolean, error: string }>()
const emit = defineEmits<{ apply: [patch: Partial<Omit<WarmupOverride, 'accountId'>>] }>()
const open = defineModel<boolean>({ required: true })
const participation = ref('keep')
const timeMode = ref('keep')
const modelMode = ref('keep')
const effortMode = ref('keep')
const delayMode = ref('keep')
const time = ref('08:00')
const model = ref('')
const effort = ref<WarmupReasoningEffort>('auto')
const delay = ref(30)
const modes = [{ value: 'keep', label: '保持不变' }, { value: 'inherit', label: '跟随全局' }, { value: 'custom', label: '独立设置' }]
const patch = computed(() => {
  const value: Partial<Omit<WarmupOverride, 'accountId'>> = {}
  if (participation.value !== 'keep')
    value.enabled = participation.value === 'enabled'
  if (timeMode.value !== 'keep')
    value.dailyTime = timeMode.value === 'inherit' ? null : time.value
  if (modelMode.value !== 'keep')
    value.model = modelMode.value === 'inherit' ? null : model.value
  if (effortMode.value !== 'keep')
    value.reasoningEffort = effortMode.value === 'inherit' ? null : effort.value
  if (delayMode.value !== 'keep')
    value.resetDelaySeconds = delayMode.value === 'inherit' ? null : delay.value
  return value
})
const validation = computed(() => {
  if (modelMode.value === 'custom') {
    const error = warmupRequestError({ model: model.value, prompt: 'hi', reasoningEffort: effort.value })
    if (error)
      return error
  }
  if (timeMode.value === 'custom' && !/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(time.value))
    return '每日时间格式为 HH:mm'
  if (delayMode.value === 'custom' && (!Number.isSafeInteger(delay.value) || delay.value < 0 || delay.value > 86400))
    return '重置延迟须为 0 至 86400 秒的整数'
  return ''
})
watch(open, (value) => {
  if (value) {
    participation.value = 'keep'
    timeMode.value = 'keep'
    modelMode.value = 'keep'
    effortMode.value = 'keep'
    delayMode.value = 'keep'
  }
})
</script>

<template>
  <BaseModal v-model="open" title="批量管理自动预热" :description="`调整已勾选的 ${count} 个账号`" size="md-wide">
    <p class="mt-0 mb-4 text-cp-sm text-cp-text-secondary">
      只修改明确选择的字段，其他独立设置保持原值。应用到草稿后，点击页面“保存设置”生效。
    </p>
    <div class="grid items-start gap-4 sm:grid-cols-2">
      <BaseFormItem label="自动参与">
        <BaseSelect v-model="participation" class="w-full" aria-label="批量自动参与" :options="[{ value: 'keep', label: '保持不变' }, { value: 'enabled', label: '参与自动预热' }, { value: 'disabled', label: '不参与自动预热' }]" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem label="每日执行时间">
        <BaseSelect v-model="timeMode" class="w-full" aria-label="批量时间模式" :options="modes" :disabled="disabled" />
        <BaseInput v-if="timeMode === 'custom'" v-model="time" class="mt-2 w-full" type="time" aria-label="批量每日时间" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem label="预热模型">
        <BaseSelect v-model="modelMode" class="w-full" aria-label="批量模型模式" :options="modes" :disabled="disabled" />
        <BaseInput v-if="modelMode === 'custom'" v-model="model" class="mt-2 w-full" aria-label="批量模型 ID" placeholder="输入模型 ID" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem label="推理强度">
        <BaseSelect v-model="effortMode" class="w-full" aria-label="批量强度模式" :options="modes" :disabled="disabled" />
        <BaseSelect v-if="effortMode === 'custom'" v-model="effort" class="mt-2 w-full" aria-label="批量推理强度" :options="warmupEffortOptions()" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem label="重置后延迟（秒）">
        <BaseSelect v-model="delayMode" class="w-full" aria-label="批量延迟模式" :options="modes" :disabled="disabled" />
        <BaseInput v-if="delayMode === 'custom'" :model-value="String(delay)" class="mt-2 w-full" type="number" min="0" max="86400" aria-label="批量重置延迟" :disabled="disabled" @update:model-value="delay = $event === '' ? Number.NaN : Number($event)" />
      </BaseFormItem>
    </div>
    <p v-if="validation || error" role="alert" class="mt-4 mb-0 text-cp-sm text-cp-error-text">
      {{ validation || error }}
    </p>
    <template #footer>
      <BaseButton @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :disabled="disabled || !props.count || !Object.keys(patch).length || Boolean(validation)" @click="emit('apply', patch)">
        应用到 {{ count }} 个账号
      </BaseButton>
    </template>
  </BaseModal>
</template>

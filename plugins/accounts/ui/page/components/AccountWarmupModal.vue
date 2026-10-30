<script setup lang="ts">
import type { WarmupAccount, WarmupModel, WarmupRequest } from '@/api/modules/accountWarmup'
import { Flame } from '@lucide/vue'
import { computed } from 'vue'
import { warmupEffortOptions, warmupRequestError } from '@/api/modules/accountWarmup'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

const props = defineProps<{
  targetName: string
  ready: boolean
  submitting: boolean
  locked: boolean
  unresolved: boolean
  error: string
  models: WarmupModel[]
  accounts: WarmupAccount[]
  catalogAccountId: string
  modelsLoading: boolean
  modelsError: string
}>()
const emit = defineEmits<{ submit: [], catalog: [accountId: string] }>()
const open = defineModel<boolean>('open', { required: true })
const request = defineModel<WarmupRequest>({ required: true })
const validation = computed(() => warmupRequestError(request.value))
const modelOptions = computed(() => {
  const options = new Map(props.models.map(item => [item.id, { value: item.id, label: item.label || item.id }]))
  if (request.value.model && !options.has(request.value.model))
    options.set(request.value.model, { value: request.value.model, label: `${request.value.model}（当前设置）` })
  return [...options.values()]
})
const efforts = computed(() => warmupEffortOptions(props.models.find(item => item.id === request.value.model), request.value.reasoningEffort))
const catalogOptions = computed(() => [{ value: '', label: '选择账号读取可用模型' }, ...props.accounts.filter(item => item.supported && item.enabled).map(item => ({ value: item.id, label: item.id }))])
</script>

<template>
  <BaseModal v-model="open" title="手动预热账号" :description="`目标：${targetName}`" size="md-wide" :dismissible="!submitting">
    <div class="flex flex-col gap-5">
      <p class="m-0 rounded-cp bg-cp-fill-quaternary px-4 py-3 text-cp-sm text-cp-text-secondary">
        每个目标账号发送一次消息，使用下面的本次参数。已有排队任务沿用原参数，自动预热计划独立保存。
      </p>
      <div class="grid gap-4 sm:grid-cols-2">
        <BaseFormItem label="预热模型">
          <BaseInput v-model="request.model" aria-label="本次预热模型 ID" :disabled="locked" />
        </BaseFormItem>
        <BaseFormItem label="推理强度" description="自动选择该模型支持的最低强度。">
          <BaseSelect v-model="request.reasoningEffort" :options="efforts" aria-label="本次预热推理强度" :disabled="locked" />
        </BaseFormItem>
        <BaseFormItem label="读取账号模型目录">
          <BaseSelect :model-value="catalogAccountId" searchable :options="catalogOptions" aria-label="手动预热模型目录账号" :disabled="locked" @update:model-value="emit('catalog', $event)" />
        </BaseFormItem>
        <BaseFormItem label="从目录选择模型">
          <BaseSelect v-model="request.model" searchable :options="modelOptions" aria-label="从目录选择本次预热模型" :disabled="locked || modelsLoading || !models.length" />
        </BaseFormItem>
      </div>
      <p v-if="modelsError" role="status" class="m-0 text-cp-xs text-cp-warning-text">
        {{ modelsError }}
      </p>
      <BaseFormItem label="预热提示词" description="默认为 hi，最多 4096 字节。所有目标使用相同的本次提示词。">
        <BaseTextarea v-model="request.prompt" aria-label="本次预热提示词" :rows="4" :disabled="locked" />
      </BaseFormItem>
      <p class="m-0 text-cp-xs text-cp-text-secondary">
        上游受理后，观测到真实的 5 小时额度窗口才标记“已预热”。受额度、养号保护或冷却限制的账号会跳过。
      </p>
      <p v-if="unresolved" role="status" class="m-0 text-cp-sm text-cp-warning-text">
        上次提交结果尚未确认。再次点击会核对同一批次，当前参数已锁定。
      </p>
      <p v-if="error || validation" role="alert" class="m-0 text-cp-sm text-cp-error-text">
        {{ error || validation }}
      </p>
    </div>
    <template #footer>
      <BaseButton :disabled="submitting" @click="open = false">
        关闭
      </BaseButton>
      <BaseButton variant="primary" :loading="submitting" :disabled="!ready || submitting || (!unresolved && Boolean(validation))" @click="emit('submit')">
        <Flame class="size-4" /> {{ unresolved ? '核对提交结果' : '开始预热' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>

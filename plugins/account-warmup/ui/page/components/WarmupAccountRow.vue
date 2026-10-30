<script setup lang="ts">
import type { WarmupAccount, WarmupModel, WarmupOverride, WarmupReasoningEffort, WarmupSettings } from '@/api/modules/accountWarmup'
import { ChevronDown, RefreshCw } from '@lucide/vue'
import { computed, ref, useId } from 'vue'
import { warmupEffortOptions } from '@/api/modules/accountWarmup'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { accountAvailability, accountOverride, formatWarmupTime } from '../model'

const props = defineProps<{
  account: WarmupAccount
  name: string
  settings: WarmupSettings
  selected: boolean
  showSelection?: boolean
  disabled: boolean
  models: WarmupModel[]
  modelsLoading: boolean
  modelsError: string
}>()
const emit = defineEmits<{
  select: [selected: boolean]
  override: [value: WarmupOverride]
  models: [accountId: string, model: string]
}>()
const expanded = ref(false)
const timeHelpId = `${useId()}-warmup-time-help`
const override = computed(() => accountOverride(props.settings, props.account.id))
const effectiveModel = computed(() => override.value.model ?? props.settings.defaultModel)
const effectiveEffort = computed(() => override.value.reasoningEffort ?? props.settings.reasoningEffort)
const effortOptions = computed(() => warmupEffortOptions(props.models.find(item => item.id === effectiveModel.value), effectiveEffort.value))
const effortLabel = computed(() => effortOptions.value.find(item => item.value === effectiveEffort.value)?.label ?? effectiveEffort.value)
const modelOptions = computed(() => {
  const entries = new Map(props.models.map(item => [item.id, { value: item.id, label: item.label || item.id }]))
  if (effectiveModel.value && !entries.has(effectiveModel.value))
    entries.set(effectiveModel.value, { value: effectiveModel.value, label: `${effectiveModel.value}（已设置）` })
  return [...entries.values()]
})
function update(value: Partial<WarmupOverride>) {
  emit('override', { ...override.value, ...value })
}
function updateEffort(value: string) {
  if (effortOptions.value.some(item => item.value === value && !item.disabled))
    update({ reasoningEffort: value as WarmupReasoningEffort })
}
function toggle() {
  expanded.value = !expanded.value
  if (expanded.value && props.account.supported && !props.models.length)
    emit('models', props.account.id, effectiveModel.value)
}
</script>

<template>
  <article class="rounded-cp border border-solid border-cp-border-secondary bg-cp-bg-container">
    <div class="flex items-center gap-3 p-4">
      <BaseCheckbox v-if="showSelection || settings.targetMode === 'selected'" :model-value="selected" :label="`选择 ${name}`" :disabled="disabled || (!account.supported && !selected)" @update:model-value="emit('select', $event)" />
      <button type="button" class="flex min-w-0 flex-1 cursor-pointer items-center gap-3 border-0 bg-transparent p-0 text-left text-cp-text" :aria-expanded="expanded" @click="toggle">
        <span class="min-w-0 flex-1">
          <span class="block truncate text-cp font-bold">{{ name }}</span>
          <span class="mt-1 block text-cp-xs text-cp-text-secondary">{{ account.provider }} · {{ accountAvailability(account) }}</span>
        </span>
        <span class="hidden text-right text-cp-xs text-cp-text-secondary sm:block">
          {{ override.enabled ? (override.dailyTime ?? `默认 ${settings.dailyTime}`) : '不自动参与' }}
          <span class="mt-1 block max-w-52 truncate">{{ effectiveModel }} · {{ effortLabel }}</span>
        </span>
        <ChevronDown class="size-4 shrink-0 transition-transform" :class="expanded ? 'rotate-180' : ''" />
      </button>
    </div>
    <div v-if="expanded" class="grid items-start gap-4 border-0 border-t border-solid border-cp-border-secondary bg-cp-fill-quaternary p-4 sm:grid-cols-2">
      <div class="flex flex-wrap items-center justify-between gap-3 sm:col-span-2">
        <BaseSwitch :model-value="override.enabled" :label="`${name} 自动参与`" show-label :disabled="disabled || !account.supported" @update:model-value="update({ enabled: $event })" />
        <span class="text-cp-xs text-cp-text-secondary">关闭仅影响自动任务，手动选择仍可预热</span>
      </div>
      <BaseFormItem label="每日执行时间">
        <div class="flex min-h-6 items-center">
          <BaseCheckbox :model-value="override.dailyTime !== null" :label="`${name} 使用独立时间`" show-label :disabled="disabled || !account.supported" @update:model-value="update({ dailyTime: $event ? settings.dailyTime : null })">
            <template #label>
              使用独立时间
            </template>
          </BaseCheckbox>
        </div>
        <BaseInput v-if="override.dailyTime !== null" class="mt-2 w-full" :model-value="override.dailyTime" type="time" :aria-label="`${name} 独立时间`" :aria-describedby="timeHelpId" :disabled="disabled" @update:model-value="update({ dailyTime: $event })" />
        <p v-else class="mt-2 mb-0 flex h-cp-control items-center text-cp-xs text-cp-text-secondary">
          跟随默认 {{ settings.dailyTime }}
        </p>
        <p :id="timeHelpId" class="mt-2 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
          使用 {{ settings.timezone }} 时区
        </p>
      </BaseFormItem>
      <BaseFormItem label="重置后延迟">
        <div class="flex min-h-6 items-center">
          <BaseCheckbox :model-value="override.resetDelaySeconds !== null" :label="`${name} 使用独立重置延迟`" show-label :disabled="disabled || !account.supported" @update:model-value="update({ resetDelaySeconds: $event ? settings.resetDelaySeconds : null })">
            <template #label>
              使用独立重置延迟
            </template>
          </BaseCheckbox>
        </div>
        <BaseInput v-if="override.resetDelaySeconds !== null" class="mt-2 w-full" :model-value="String(override.resetDelaySeconds)" type="number" min="0" max="86400" :aria-label="`${name} 独立重置延迟秒数`" :disabled="disabled" @update:model-value="update({ resetDelaySeconds: $event === '' ? Number.NaN : Number($event) })" />
        <p v-else class="mt-2 mb-0 flex h-cp-control items-center text-cp-xs text-cp-text-secondary">
          跟随默认 {{ settings.resetDelaySeconds }} 秒
        </p>
      </BaseFormItem>
      <BaseFormItem label="预热模型" class="sm:col-span-2">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <BaseCheckbox :model-value="override.model !== null" :label="`${name} 使用独立模型`" show-label :disabled="disabled || !account.supported" @update:model-value="update({ model: $event ? settings.defaultModel : null })">
            <template #label>
              使用独立模型
            </template>
          </BaseCheckbox>
          <BaseButton size="sm" variant="ghost" :loading="modelsLoading" :disabled="disabled || !account.supported" @click="emit('models', account.id, effectiveModel)">
            <RefreshCw class="size-3.5" /> 模型目录
          </BaseButton>
        </div>
        <div v-if="override.model !== null" class="mt-2 grid gap-2 sm:grid-cols-2">
          <BaseInput class="w-full" :model-value="override.model" :aria-label="`${name} 独立模型 ID`" :disabled="disabled" @update:model-value="update({ model: $event })" />
          <BaseSelect class="w-full" :model-value="override.model" searchable :options="modelOptions" :aria-label="`${name} 从目录选择模型`" :disabled="disabled || modelsLoading" @update:model-value="update({ model: $event })" />
        </div>
        <p v-else class="mt-2 mb-0 text-cp-xs text-cp-text-secondary">
          跟随默认 {{ settings.defaultModel }}
        </p>
        <p v-if="modelsError" role="alert" class="mt-2 mb-0 text-cp-xs text-cp-error-text">
          {{ modelsError }}
        </p>
      </BaseFormItem>
      <BaseFormItem label="推理强度" class="sm:col-span-2">
        <BaseCheckbox :model-value="override.reasoningEffort !== null" :label="`${name} 使用独立推理强度`" show-label :disabled="disabled || !account.supported" @update:model-value="update({ reasoningEffort: $event ? settings.reasoningEffort : null })">
          <template #label>
            使用独立推理强度
          </template>
        </BaseCheckbox>
        <BaseSelect v-if="override.reasoningEffort !== null" class="mt-2 w-full" :model-value="override.reasoningEffort" :options="effortOptions" :aria-label="`${name} 独立推理强度`" :disabled="disabled || modelsLoading" @update:model-value="updateEffort" />
        <p v-else class="mt-2 mb-0 text-cp-xs text-cp-text-secondary">
          跟随默认 {{ effortLabel }}
        </p>
      </BaseFormItem>
      <p class="m-0 text-cp-xs text-cp-text-secondary sm:col-span-2">
        真实短窗口重置时间：{{ formatWarmupTime(account.shortTerm?.resetAt) }}。未观测到 resetAt 时不推算计时。
      </p>
    </div>
  </article>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { parseQuotaProtectionLimit, QUOTA_PROTECTION_PRESETS } from '../utils/quotaProtection'

const props = defineProps<{ supported: boolean, disabled: boolean, split: boolean }>()
const enabled = defineModel<boolean>('enabled', { required: true })
const usedPercentLimit = defineModel<string>('usedPercentLimit', { required: true })
const shortTermUsedPercentLimit = defineModel<string>('shortTermUsedPercentLimit', { required: true })
const weeklyUsedPercentLimit = defineModel<string>('weeklyUsedPercentLimit', { required: true })
const fields = computed(() => props.split
  ? [
      { key: 'shortTerm', label: '5 小时', value: shortTermUsedPercentLimit.value },
      { key: 'weekly', label: '7 天', value: weeklyUsedPercentLimit.value },
    ]
  : [{ key: 'billing', label: '账期', value: usedPercentLimit.value }])

function updateLimit(key: string, value: string) {
  if (key === 'shortTerm')
    shortTermUsedPercentLimit.value = value
  else if (key === 'weekly')
    weeklyUsedPercentLimit.value = value
  else
    usedPercentLimit.value = value
}
</script>

<template>
  <section class="grid gap-3 rounded-cp border border-cp-border-light bg-cp-fill-quaternary p-4" aria-label="养号设置">
    <div class="flex items-center justify-between gap-3">
      <span class="font-emphasis text-cp-text">养号</span>
      <BaseSwitch v-model="enabled" label="养号" :disabled="disabled || !supported" />
    </div>
    <p v-if="!supported" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
      当前网关不支持{{ split ? '分周期' : '' }}养号设置，请先更新网关；此处无法保存养号设置。
    </p>
    <template v-else-if="enabled">
      <div v-for="field in fields" :key="field.key" class="grid gap-2">
        <div class="flex flex-wrap items-center gap-2 text-cp-sm text-cp-text-secondary">
          <span>{{ field.label }}使用达到</span>
          <BaseInput
            :model-value="field.value"
            class="w-20"
            :aria-label="`${field.label}养号已使用百分比阈值`"
            type="number"
            min="1"
            max="99"
            step="1"
            :disabled="disabled"
            :aria-invalid="parseQuotaProtectionLimit(field.value) === null"
            @update:model-value="updateLimit(field.key, $event)"
          />
          <span>% 后暂停分配新请求<span v-if="parseQuotaProtectionLimit(field.value) !== null">，目标保留 {{ 100 - parseQuotaProtectionLimit(field.value)! }}%</span></span>
        </div>
        <div class="flex flex-wrap items-center gap-2 text-cp-sm text-cp-text-secondary">
          <span>常用比例</span>
          <BaseButton
            v-for="preset in QUOTA_PROTECTION_PRESETS"
            :key="preset"
            size="sm"
            :variant="parseQuotaProtectionLimit(field.value) === preset ? 'primary' : 'secondary'"
            :disabled="disabled"
            :aria-label="`${field.label}养号阈值 ${preset}%`"
            :aria-pressed="parseQuotaProtectionLimit(field.value) === preset"
            @click="updateLimit(field.key, String(preset))"
          >
            {{ preset }}%
          </BaseButton>
        </div>
        <p v-if="parseQuotaProtectionLimit(field.value) === null" role="alert" class="m-0 text-cp-sm text-cp-error">
          {{ field.label }}请输入 1 至 99 的整数百分比。
        </p>
      </div>
      <p class="m-0 text-cp-sm leading-5 text-cp-text-secondary">
        {{ split ? '任一周期达到阈值即暂停分配新请求。' : '' }}仅对有效的账号级额度窗口生效；未知额度不保证预留，已发请求继续完成。
      </p>
    </template>
  </section>
</template>

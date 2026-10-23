<script setup lang="ts">
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { parseQuotaProtection, QUOTA_PROTECTION_PRESETS } from '../utils/quotaProtection'

defineProps<{ supported: boolean, disabled: boolean }>()
const enabled = defineModel<boolean>('enabled', { required: true })
const usedPercentLimit = defineModel<string>('usedPercentLimit', { required: true })
const parsed = computed(() => parseQuotaProtection(enabled.value, usedPercentLimit.value))

function selectPreset(value: number) {
  usedPercentLimit.value = String(value)
}
</script>

<template>
  <section class="grid gap-3 rounded-cp border border-cp-border-light bg-cp-fill-quaternary p-4" aria-label="养号设置">
    <div class="flex items-center justify-between gap-3">
      <span class="font-emphasis text-cp-text">养号</span>
      <BaseSwitch v-model="enabled" label="养号" :disabled="disabled || !supported" />
    </div>
    <p v-if="!supported" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
      当前网关不支持养号设置，请先更新网关；此处无法保存养号设置。
    </p>
    <template v-else-if="enabled">
      <div class="flex flex-wrap items-center gap-2 text-cp-sm text-cp-text-secondary">
        <span>使用达到</span>
        <BaseInput
          v-model="usedPercentLimit"
          class="w-20"
          aria-label="养号已使用百分比阈值"
          type="number"
          min="1"
          max="99"
          step="1"
          :disabled="disabled"
          :aria-invalid="!parsed"
        />
        <span>% 后暂停分配新请求<span v-if="parsed">，目标保留 {{ 100 - parsed.usedPercentLimit }}%</span></span>
      </div>
      <div class="flex flex-wrap items-center gap-2 text-cp-sm text-cp-text-secondary">
        <span>常用比例</span>
        <BaseButton
          v-for="preset in QUOTA_PROTECTION_PRESETS"
          :key="preset"
          size="sm"
          :variant="parsed?.usedPercentLimit === preset ? 'primary' : 'secondary'"
          :disabled="disabled"
          :aria-pressed="parsed?.usedPercentLimit === preset"
          @click="selectPreset(preset)"
        >
          {{ preset }}%
        </BaseButton>
      </div>
      <p v-if="!parsed" role="alert" class="m-0 text-cp-sm text-cp-error">
        请输入 1 至 99 的整数百分比。
      </p>
      <p class="m-0 text-cp-sm leading-5 text-cp-text-secondary">
        仅对有效的账号级额度窗口生效；未知额度不保证预留，已发请求继续完成。
      </p>
    </template>
  </section>
</template>

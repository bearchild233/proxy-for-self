<script setup lang="ts">
import type { SubscriptionState } from '../composables/useAccountSubscriptions'
import type { Account } from '@/api'
import { computed } from 'vue'
import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{ account: Account, state?: SubscriptionState, compact?: boolean }>()
const now = useUiClock()
const supported = computed(() => props.account.provider === 'openai' && props.account.authenticationKind === 'oauth')
const expiresAt = computed(() => props.state?.subscription?.expiresAt ?? props.account.lifecycle.subscriptionExpiresAt ?? undefined)
const remaining = computed(() => expiresAt.value ? Date.parse(expiresAt.value) - now.value.getTime() : Number.NaN)
const hasDate = computed(() => Number.isFinite(remaining.value))
const expiryTitle = computed(() => {
  const expiry = hasDate.value ? `到期：${formatDateTime(expiresAt.value)}` : ''
  return props.state?.failed && hasDate.value ? `${expiry}；更新失败，显示上次数据` : expiry || undefined
})
const label = computed(() => {
  if (!supported.value)
    return '不适用'
  if (!hasDate.value) {
    if (!props.account.enabled)
      return '已停用'
    return props.state?.loading ? '读取中…' : '暂未获取'
  }
  if (remaining.value <= 0)
    return '本期已结束'
  const minutes = Math.ceil(remaining.value / 60_000)
  const days = Math.floor(minutes / 1440)
  const hours = Math.floor(minutes % 1440 / 60)
  return days ? `剩余 ${days} 天 ${hours} 小时` : hours ? `剩余 ${hours} 小时 ${minutes % 60} 分钟` : `剩余 ${minutes} 分钟`
})
</script>

<template>
  <div class="grid gap-1 text-cp-xs">
    <span class="font-emphasis" :class="hasDate && remaining < 3 * 86_400_000 ? 'text-cp-warning-text' : 'text-cp-text'" :title="expiryTitle">{{ label }}</span>
    <template v-if="!compact">
      <time v-if="hasDate" :datetime="expiresAt" class="whitespace-nowrap text-cp-text-secondary">{{ formatDateTime(expiresAt) }}</time>
      <span v-if="state?.failed && hasDate" class="text-cp-text-secondary">更新失败，显示上次数据</span>
      <span v-if="supported && hasDate" class="text-cp-text-secondary">{{ state?.subscription?.willRenew === true ? '自动续费' : state?.subscription?.willRenew === false ? '不自动续费' : '续费状态未知' }}</span>
    </template>
  </div>
</template>

<script setup lang="ts">
import type { WarmupAccountState } from '@/api/modules/accountWarmup'
import { Flame } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{ state?: WarmupAccountState, ready: boolean, now: number, archived?: boolean }>()
const status = computed(() => {
  if (props.archived)
    return 'archived'
  if (!props.ready || !props.state)
    return 'unavailable'
  if (props.state.status === 'warmed' && (!props.state.resetAt || !Number.isFinite(Date.parse(props.state.resetAt)) || Date.parse(props.state.resetAt) <= props.now))
    return 'pending'
  return props.state.status
})
const labels = {
  pending: '待预热',
  queued: '等待预热',
  running: '预热中',
  warmed: '已预热',
  unconfirmed: '计时待确认',
  unknown: '结果未知',
  failed: '预热失败',
  skipped: '已跳过',
  unsupported: '不支持预热',
  unavailable: '状态不可用',
  archived: '已归档',
}
const description = computed(() => {
  if (status.value === 'warmed')
    return `当前 5 小时额度窗口已启动，重置于 ${new Date(props.state!.resetAt!).toLocaleString('zh-CN', { hour12: false })}`
  if (status.value === 'unconfirmed')
    return '上游已受理，尚未观测到当前 5 小时窗口；不会虚构倒计时'
  if (status.value === 'unknown')
    return '上次请求结果未知，后台不会自动重发'
  if (status.value === 'pending')
    return '当前没有已确认的有效 5 小时窗口'
  if (status.value === 'unavailable')
    return '无法读取当前预热状态，请检查预热插件并刷新'
  return labels[status.value] + (props.state?.reasonCode ? ` · ${props.state.reasonCode}` : '')
})
</script>

<template>
  <span
    class="mt-1 inline-flex items-center gap-1 rounded-cp-sm px-1.5 py-0.5 text-cp-xs leading-normal"
    :class="status === 'warmed' ? 'bg-cp-success-container text-cp-success-on-container' : status === 'running' || status === 'queued' ? 'bg-cp-primary-container text-cp-primary-on-container' : ['unknown', 'unconfirmed', 'failed'].includes(status) ? 'bg-cp-warning-container text-cp-warning-text' : 'bg-cp-fill-quaternary text-cp-text-secondary'"
    :title="description"
    role="status"
  >
    <Flame class="size-3" :class="status === 'running' ? 'animate-pulse motion-reduce:animate-none' : ''" />
    {{ labels[status] }}
  </span>
</template>

<script setup lang="ts">
import type { WarmupRun, WarmupTask } from '@/api/modules/accountWarmup'
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { formatWarmupTime, reasonText, sourceLabels, taskLabels } from '../model'

const props = defineProps<{
  runs: WarmupRun[]
  items: WarmupTask[]
  selectedId: string
  names: Map<string, { name: string }>
  error: string
  cancelling: boolean
}>()
const emit = defineEmits<{ select: [id: string], refresh: [], cancel: [] }>()
const selected = computed(() => props.runs.find(run => run.id === props.selectedId))
const options = computed(() => props.runs.map(run => ({ value: run.id, label: `${sourceLabels[run.source]} · ${formatWarmupTime(run.createdAt)} · ${run.total} 个账号` })))
const canCancel = computed(() => selected.value && ['queued', 'running'].includes(selected.value.status) && selected.value.pending > 0)
</script>

<template>
  <BaseCard title="最近预热任务" description="仅收到上游响应头时记为受理；是否已开启计时另看真实额度观测。">
    <template #actions>
      <BaseButton size="sm" @click="emit('refresh')">
        刷新记录
      </BaseButton>
    </template>
    <p v-if="error" role="alert" class="m-0 mb-3 text-cp-sm text-cp-error-text">
      {{ error }}
    </p>
    <template v-if="runs.length">
      <div class="flex flex-wrap items-center gap-3">
        <BaseSelect class="min-w-56 flex-1" :model-value="selectedId" :options="options" aria-label="查看预热批次" @update:model-value="emit('select', $event)" />
        <BaseButton v-if="canCancel" size="sm" :loading="cancelling" @click="emit('cancel')">
          停止未发送项
        </BaseButton>
      </div>
      <p v-if="selected" class="my-3 text-cp-sm text-cp-text-secondary" role="status">
        {{ selected.status === 'completed' ? '已结束' : selected.status === 'cancelled' ? '已停止' : selected.status === 'running' ? '执行中' : '已加入队列' }} · 总数 {{ selected.total }} · 收头 {{ selected.accepted }} · 失败 {{ selected.failed }} · 跳过 {{ selected.skipped }} · 未知 {{ selected.unknown }} · 未完成 {{ selected.pending }}
      </p>
      <div class="grid gap-2">
        <article v-for="item in items" :key="item.id" class="grid min-w-0 gap-2 rounded-cp bg-cp-fill-quaternary px-4 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)]">
          <div class="min-w-0">
            <span class="block truncate text-cp-sm font-bold">{{ names.get(item.accountId)?.name || item.accountId }}</span>
            <span class="mt-1 block truncate font-mono text-cp-xs text-cp-text-secondary">{{ item.model }}</span>
            <span class="mt-1 block text-cp-xs text-cp-text-secondary">{{ taskLabels[item.status] }}<template v-if="item.httpStatus"> · HTTP {{ item.httpStatus }}</template><template v-if="item.durationMs !== null"> · {{ item.durationMs }} ms</template></span>
          </div>
          <div class="text-cp-xs text-cp-text-secondary">
            <span class="block" :class="item.timerConfirmed ? 'text-cp-success-text' : ''">{{ item.timerConfirmed ? '计时已确认' : '计时尚未确认' }} · resetAt {{ formatWarmupTime(item.resetAt) }}</span>
            <span class="mt-1 block">{{ item.startedAt ? '发送于' : '计划于' }} {{ formatWarmupTime(item.startedAt || item.dueAt) }}</span>
            <span v-if="item.reasonCode" class="mt-1 block">{{ reasonText(item.reasonCode) }}</span>
            <span v-if="item.status === 'unknown'" class="mt-1 block text-cp-warning-text">结果未知，不自动重发</span>
          </div>
        </article>
      </div>
      <p v-if="!items.length" class="my-3 text-cp-sm text-cp-text-secondary">
        该批次暂无逐账号结果
      </p>
    </template>
    <BaseEmpty v-else title="暂无预热任务" description="从账号管理点击预热，或保存并开启自动计划。" size="sm" surface="none" />
  </BaseCard>
</template>

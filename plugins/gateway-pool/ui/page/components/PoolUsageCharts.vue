<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { GatewaySummary, RecentWindow } from '../history'
import { computed } from 'vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { duration } from '../model'

const props = defineProps<{ gateways: GatewaySummary[], recent: RecentWindow[], names: Map<string, string> }>()
const emit = defineEmits<{ gateway: [value: string], window: [value: { accountId: string, gateway: string }] }>()
const { palette } = useChartPalette()
const colors = computed(() => [palette.value.info, palette.value.success, palette.value.warning, palette.value.reasoning, palette.value.danger])
const slices = computed(() => [...props.gateways].sort((a, b) => b.windowSeconds - a.windowSeconds).map((row, index) => ({ ...row, color: colors.value[index % colors.value.length] })))
const total = computed(() => slices.value.reduce((sum, row) => sum + row.windowSeconds, 0))
const missing = computed(() => slices.value.reduce((sum, row) => sum + row.missing.windowSeconds, 0))
const ringOption = computed<EChartsOption>(() => ({
  animation: false,
  tooltip: { trigger: 'item', renderMode: 'richText', formatter: '{b}\n{c} 秒 · {d}%' },
  series: [{
    type: 'pie',
    radius: ['68%', '88%'],
    label: { show: false },
    data: slices.value.filter(row => row.windowSeconds > 0).map(row => ({ name: row.gateway, value: row.windowSeconds, itemStyle: { color: row.color, borderColor: palette.value.surface, borderWidth: 2 } })),
  }],
}))
// 最近 24 小时每个账号各网关的窗口时长，按账号分组，组内从长到短。
const recentGroups = computed(() => {
  const groups = new Map<string, { accountId: string, total: number, rows: RecentWindow[] }>()
  for (const row of props.recent) {
    const group = groups.get(row.accountId) ?? { accountId: row.accountId, total: 0, rows: [] }
    group.total += row.windowSeconds
    group.rows.push(row)
    groups.set(row.accountId, group)
  }
  return [...groups.values()].map(group => ({ ...group, rows: [...group.rows].sort((a, b) => b.windowSeconds - a.windowSeconds) }))
})
const recentMax = computed(() => Math.max(1, ...props.recent.map(row => row.windowSeconds)))
</script>

<template>
  <div class="grid min-w-0 gap-4 xl:grid-cols-2">
    <section class="min-w-0 rounded-cp-card bg-cp-bg-container p-4 shadow-cp-card" aria-label="账号网关窗口时长分布">
      <div class="mb-4 flex flex-wrap items-center justify-between gap-2">
        <h4 class="text-cp-sm font-emphasis">
          网关窗口时长分布
        </h4>
      </div>
      <div class="flex flex-wrap items-center gap-5">
        <div v-if="total > 0" class="relative mx-auto w-40 shrink-0" aria-hidden="true">
          <BaseChart :option="ringOption" :height="176" />
          <div class="pointer-events-none absolute inset-0 flex flex-col items-center justify-center">
            <strong class="text-2xl font-emphasis">{{ slices.filter(row => row.windowSeconds > 0).length }}</strong>
            <span class="text-cp-xs text-cp-text-secondary">使用网关</span>
          </div>
        </div>
        <BaseEmpty v-else size="sm" surface="none" title="暂无已知时长" />
        <div class="max-h-64 min-w-48 flex-1 overflow-auto">
          <button v-for="row in slices" :key="row.gateway" class="flex w-full items-center gap-3 rounded-cp px-2 py-2.5 text-left hover:bg-cp-fill-tertiary" type="button" @click="emit('gateway', row.gateway)">
            <span class="size-2 shrink-0 rounded-full" :style="{ backgroundColor: row.color }" />
            <span class="min-w-0 flex-1">
              <span class="block font-mono text-cp-sm">{{ row.gateway }}</span>
              <span class="text-cp-xs text-cp-text-secondary">{{ duration(row.windowSeconds) }}</span>
            </span>
            <span class="text-cp-xs tabular-nums text-cp-text-secondary">{{ total ? `${(row.windowSeconds / total * 100).toFixed(1)}%` : '—' }}</span>
          </button>
        </div>
      </div>
      <p class="mt-3 text-cp-xs text-cp-text-secondary">
        窗口总时长 {{ duration(total) }}<span v-if="missing"> · {{ missing }} 条含缺失数据，比例为已知时长占比</span>
      </p>
    </section>
    <section class="min-w-0 rounded-cp-card bg-cp-bg-container p-4 shadow-cp-card" aria-label="最近 24 小时各账号网关窗口时长">
      <div class="mb-3 flex items-center justify-between gap-2">
        <h4 class="text-cp-sm font-emphasis">
          最近 24 小时窗口时长
        </h4>
        <span class="text-cp-xs text-cp-text-secondary">每个账号各网关</span>
      </div>
      <div v-if="recentGroups.length" class="max-h-80 space-y-3 overflow-auto">
        <div v-for="group in recentGroups" :key="group.accountId">
          <div class="mb-1 flex items-center justify-between gap-2 text-cp-xs">
            <span class="font-emphasis text-cp-text">{{ names.get(group.accountId) ?? group.accountId }}</span>
            <span class="tabular-nums text-cp-text-secondary">合计 {{ duration(group.total) }}</span>
          </div>
          <button v-for="row in group.rows" :key="row.gateway" type="button" class="flex w-full items-center gap-2 rounded-cp px-1 py-1 text-left hover:bg-cp-fill-tertiary" @click="emit('window', { accountId: row.accountId, gateway: row.gateway })">
            <span class="w-24 shrink-0 font-mono text-cp-xs">{{ row.gateway }}</span>
            <span class="h-1.5 flex-1 overflow-hidden rounded-full bg-cp-fill-tertiary">
              <i class="block h-full rounded-full bg-cp-info" :style="{ width: `${Math.max(2, row.windowSeconds / recentMax * 100)}%` }" />
            </span>
            <span class="w-20 shrink-0 text-right text-cp-xs tabular-nums text-cp-text-secondary">{{ duration(row.windowSeconds) }}</span>
          </button>
        </div>
      </div>
      <BaseEmpty v-else size="sm" surface="none" title="最近 24 小时暂无窗口" />
      <p class="mt-3 text-cp-xs text-cp-text-secondary">
        不受上方账号与时间筛选影响，口径同窗口时长
      </p>
    </section>
  </div>
</template>

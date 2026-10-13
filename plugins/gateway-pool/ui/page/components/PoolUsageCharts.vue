<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { GatewaySummary, UsageBatch } from '../history'
import { computed } from 'vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { duration } from '../model'

const props = defineProps<{ gateways: GatewaySummary[], batches: UsageBatch[] }>()
const emit = defineEmits<{ gateway: [value: string], batch: [value: string] }>()
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
const batchOption = computed<EChartsOption>(() => ({
  animation: false,
  tooltip: { trigger: 'axis', renderMode: 'richText', valueFormatter: value => duration(Number(value)) },
  legend: { top: 0, textStyle: { color: palette.value.textSecondary, fontSize: 12 } },
  grid: { left: 70, right: 24, top: 40, bottom: 28 },
  xAxis: { type: 'value', name: '秒', axisLabel: { color: palette.value.textMuted }, splitLine: { lineStyle: { color: palette.value.grid } } },
  yAxis: { type: 'category', inverse: true, data: props.batches.map(batch => `第 ${batch.number} 批`), axisLabel: { color: palette.value.textPrimary }, axisLine: { show: false }, axisTick: { show: false } },
  series: [{
    name: '窗口总时长',
    type: 'bar',
    barMaxWidth: 16,
    itemStyle: { color: palette.value.info },
    data: props.batches.map(batch => batch.windowSeconds),
  }],
}))
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
    <section class="min-w-0 rounded-cp-card bg-cp-bg-container p-4 shadow-cp-card" aria-label="账号每批轮转时长">
      <div class="mb-3 flex items-center justify-between gap-2">
        <h4 class="text-cp-sm font-emphasis">
          每批窗口总时长
        </h4>
        <span class="text-cp-xs text-cp-text-secondary">本页批次</span>
      </div>
      <div v-if="batches.length" class="max-h-80 overflow-auto">
        <BaseChart :option="batchOption" :height="Math.max(220, batches.length * 55 + 70)" />
      </div>
      <BaseEmpty v-else size="sm" surface="none" title="当前范围暂无批次" />
      <div class="mt-3 flex flex-wrap gap-2">
        <button v-for="batch in batches" :key="batch.id" type="button" class="rounded-cp bg-cp-fill-tertiary px-2 py-1 text-cp-xs text-cp-text-secondary hover:text-cp-primary-text" @click="emit('batch', batch.id)">
          查看第 {{ batch.number }} 批
        </button>
      </div>
    </section>
  </div>
</template>

<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { CoolingAnalysis } from '../history'
import { computed } from 'vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { duration } from '../model'

const props = defineProps<{ data: CoolingAnalysis }>()
const { palette } = useChartPalette()
const judged = computed(() => props.data.recovered + props.data.unrecovered)
const rate = computed(() => judged.value ? `${(props.data.recovered / judged.value * 100).toFixed(1)}%` : '—')
const option = computed<EChartsOption>(() => ({
  animation: false,
  tooltip: {
    trigger: 'axis',
    renderMode: 'richText',
    formatter: (params) => {
      const item = Array.isArray(params) ? params[0] : params
      const bin = props.data.bins[item?.dataIndex ?? 0]
      return bin ? `${bin.label}\n已恢复 ${bin.recovered} · 未恢复 ${bin.unrecovered}\n访问失败 ${bin.failed} · 待结果 ${bin.pending}` : ''
    },
  },
  grid: { left: 48, right: 16, top: 28, bottom: 52 },
  xAxis: {
    type: 'category',
    data: props.data.bins.map(bin => `${bin.label}\n${bin.recovered + bin.unrecovered} 次判定`),
    axisLabel: { color: palette.value.textSecondary, fontSize: 11, interval: 0 },
    axisTick: { show: false },
    axisLine: { show: false },
  },
  yAxis: {
    type: 'value',
    min: 0,
    max: 100,
    axisLabel: { color: palette.value.textMuted, formatter: '{value}%' },
    splitLine: { lineStyle: { color: palette.value.grid } },
  },
  series: [{
    name: '观测恢复比例',
    type: 'bar',
    barMaxWidth: 40,
    itemStyle: { color: palette.value.success },
    label: { show: true, position: 'top', color: palette.value.textSecondary, formatter: '{c}%' },
    data: props.data.bins.map(bin => bin.recovered + bin.unrecovered ? Number((bin.recovered / (bin.recovered + bin.unrecovered) * 100).toFixed(1)) : null),
  }],
}))
</script>

<template>
  <section class="rounded-cp-card bg-cp-bg-container p-4 shadow-cp-card" aria-label="冷却汇总">
    <div class="flex flex-wrap items-center gap-x-6 gap-y-2 text-cp-sm">
      <h4 class="font-emphasis">
        冷却汇总
      </h4>
      <template v-if="data.total">
        <span>已恢复 {{ data.recovered }} / {{ judged }} 次</span>
        <span>恢复比例 {{ rate }}</span>
        <span>恢复样本间隔中位数 {{ duration(data.recoveredMedianSeconds) }}</span>
      </template>
      <span v-else class="text-cp-text-secondary">暂无再次访问记录</span>
    </div>
    <div class="mt-3 min-w-0 overflow-x-auto" aria-label="按实际再访问间隔分组的恢复比例与判定样本数">
      <div class="min-w-80">
        <BaseChart :option="option" :height="240" />
      </div>
    </div>
    <p class="mt-2 text-cp-xs text-cp-text-tertiary">
      按实际再访问间隔分组，无判定样本留空 · 间隔不代表准确恢复时间<span v-if="data.failed + data.pending"> · {{ data.failed + data.pending }} 次失败或待结果未计入比例</span><span v-if="data.missingInterval"> · {{ data.missingInterval }} 次间隔缺失，未分组</span>
    </p>
  </section>
</template>

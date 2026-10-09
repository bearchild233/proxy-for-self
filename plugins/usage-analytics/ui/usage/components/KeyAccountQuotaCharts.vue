<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { UsageKeyAccountBreakdownItem } from '@/api'
import type { SelectOption } from '@/components/base/BaseSelect.vue'

import {
  decimalDisplayNumber,
  formatPercent,
  formatUsd,
} from '@kit/format'
import { computed, ref, watch } from 'vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatLocalizedCompactNumber as formatCompactNumber } from '@/utils/number'
import { isRecord } from '@/utils/object'
import {
  usageTooltipContent,
  usageTooltipItem,
} from '../utils/chart'

const props = withDefaults(
  defineProps<{
    items: UsageKeyAccountBreakdownItem[]
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const { palette } = useChartPalette()

const sliceColors = computed(() => {
  const p = palette.value
  return [
    p.info,
    p.success,
    p.warning,
    p.reasoning,
    p.danger,
    p.normal,
    p.textSecondary,
    p.textMuted,
  ]
})

interface MetaEntry {
  ref: string
  name: string
  totalCost: number
  totalRequests: number
}

// Group all items by key to generate key options
const keyMetas = computed<MetaEntry[]>(() => {
  const map = new Map<string, MetaEntry>()
  for (const item of props.items) {
    const ref = item.clientApiKeyRef
    let meta = map.get(ref)
    if (!meta) {
      meta = {
        ref,
        name: item.clientApiKeyName || '',
        totalCost: 0,
        totalRequests: 0,
      }
      map.set(ref, meta)
    }
    const cost = decimalDisplayNumber(item.costAmount)
    if (cost != null && cost > 0) {
      meta.totalCost += cost
    }
    meta.totalRequests += item.requestCount
    if (!meta.name && item.clientApiKeyName) {
      meta.name = item.clientApiKeyName
    }
  }

  return Array.from(map.values()).sort((a, b) => {
    if (b.totalCost !== a.totalCost)
      return b.totalCost - a.totalCost
    if (b.totalRequests !== a.totalRequests)
      return b.totalRequests - a.totalRequests
    return a.ref.localeCompare(b.ref)
  })
})

const keyOptions = computed<SelectOption[]>(() =>
  keyMetas.value.map((meta) => {
    const trimmedName = meta.name.trim()
    const trimmedRef = meta.ref.trim()
    const primary = trimmedName || trimmedRef || '未知'
    const secondary = primary !== trimmedRef && trimmedRef ? trimmedRef : undefined
    return {
      label: primary,
      value: meta.ref,
      description: secondary,
    }
  }),
)

// Group all items by provider account to generate account options
const accountMetas = computed<MetaEntry[]>(() => {
  const map = new Map<string, MetaEntry>()
  for (const item of props.items) {
    const ref = item.providerAccountRef
    let meta = map.get(ref)
    if (!meta) {
      meta = {
        ref,
        name: item.providerAccountName || '',
        totalCost: 0,
        totalRequests: 0,
      }
      map.set(ref, meta)
    }
    const cost = decimalDisplayNumber(item.costAmount)
    if (cost != null && cost > 0) {
      meta.totalCost += cost
    }
    meta.totalRequests += item.requestCount
    if (!meta.name && item.providerAccountName) {
      meta.name = item.providerAccountName
    }
  }

  return Array.from(map.values()).sort((a, b) => {
    if (b.totalCost !== a.totalCost)
      return b.totalCost - a.totalCost
    if (b.totalRequests !== a.totalRequests)
      return b.totalRequests - a.totalRequests
    return a.ref.localeCompare(b.ref)
  })
})

const accountOptions = computed<SelectOption[]>(() =>
  accountMetas.value.map((meta) => {
    const trimmedName = meta.name.trim()
    const trimmedRef = meta.ref.trim()
    const primary = trimmedName || trimmedRef || '未知'
    const secondary = primary !== trimmedRef && trimmedRef ? trimmedRef : undefined
    return {
      label: primary,
      value: meta.ref,
      description: secondary,
    }
  }),
)

const selectedKey = ref('')
const selectedAccount = ref('')

watch(
  keyOptions,
  (options) => {
    if (options.length === 0) {
      selectedKey.value = ''
      return
    }
    if (!selectedKey.value || !options.some(opt => opt.value === selectedKey.value)) {
      selectedKey.value = options[0].value
    }
  },
  { immediate: true },
)

watch(
  accountOptions,
  (options) => {
    if (options.length === 0) {
      selectedAccount.value = ''
      return
    }
    if (!selectedAccount.value || !options.some(opt => opt.value === selectedAccount.value)) {
      selectedAccount.value = options[0].value
    }
  },
  { immediate: true },
)

interface SliceItem {
  ref: string
  rawName: string
  name: string
  value: number
  requestCount: number
  currency: string
  costDisplay: string
  itemStyle: { color: string }
}

function formatCostValue(amount: number, currency?: string | null) {
  if (!Number.isFinite(amount))
    return '—'
  if (currency && currency !== 'USD') {
    const formatted = Math.abs(amount) < 0.01 && amount > 0 ? amount.toFixed(4) : amount.toFixed(2)
    return `${currency} ${formatted}`
  }
  return formatUsd(amount)
}

// Chart A slices: selected key -> distribution across provider accounts
const keyAccountSlices = computed<SliceItem[]>(() => {
  if (!selectedKey.value)
    return []

  const groupMap = new Map<string, {
    ref: string
    name: string
    cost: number
    requests: number
    currency: string
  }>()

  for (const item of props.items) {
    if (item.clientApiKeyRef !== selectedKey.value)
      continue

    const costNum = decimalDisplayNumber(item.costAmount)
    if (costNum == null || costNum <= 0)
      continue

    const ref = item.providerAccountRef
    let entry = groupMap.get(ref)
    if (!entry) {
      entry = {
        ref,
        name: item.providerAccountName || '',
        cost: 0,
        requests: 0,
        currency: item.costCurrency || 'USD',
      }
      groupMap.set(ref, entry)
    }
    entry.cost += costNum
    entry.requests += item.requestCount
    if (!entry.name && item.providerAccountName) {
      entry.name = item.providerAccountName
    }
  }

  const entries = Array.from(groupMap.values())
  if (entries.length === 0)
    return []

  const nameCounts = new Map<string, number>()
  for (const e of entries) {
    const raw = e.name.trim() || e.ref.trim() || '未知'
    nameCounts.set(raw, (nameCounts.get(raw) ?? 0) + 1)
  }

  const colors = sliceColors.value
  return entries
    .sort((a, b) => b.cost - a.cost)
    .map((e, index) => {
      const raw = e.name.trim() || e.ref.trim() || '未知'
      const isDuplicate = (nameCounts.get(raw) ?? 0) > 1
      const uniqueName = isDuplicate && e.ref.trim() !== raw ? `${raw} (${e.ref.trim()})` : raw

      return {
        ref: e.ref,
        rawName: raw,
        name: uniqueName,
        value: Number(e.cost.toFixed(6)),
        requestCount: e.requests,
        currency: e.currency,
        costDisplay: formatCostValue(e.cost, e.currency),
        itemStyle: {
          color: colors[index % colors.length],
        },
      }
    })
})

// Chart B slices: selected account -> distribution across client keys
const accountKeySlices = computed<SliceItem[]>(() => {
  if (!selectedAccount.value)
    return []

  const groupMap = new Map<string, {
    ref: string
    name: string
    cost: number
    requests: number
    currency: string
  }>()

  for (const item of props.items) {
    if (item.providerAccountRef !== selectedAccount.value)
      continue

    const costNum = decimalDisplayNumber(item.costAmount)
    if (costNum == null || costNum <= 0)
      continue

    const ref = item.clientApiKeyRef
    let entry = groupMap.get(ref)
    if (!entry) {
      entry = {
        ref,
        name: item.clientApiKeyName || '',
        cost: 0,
        requests: 0,
        currency: item.costCurrency || 'USD',
      }
      groupMap.set(ref, entry)
    }
    entry.cost += costNum
    entry.requests += item.requestCount
    if (!entry.name && item.clientApiKeyName) {
      entry.name = item.clientApiKeyName
    }
  }

  const entries = Array.from(groupMap.values())
  if (entries.length === 0)
    return []

  const nameCounts = new Map<string, number>()
  for (const e of entries) {
    const raw = e.name.trim() || e.ref.trim() || '未知'
    nameCounts.set(raw, (nameCounts.get(raw) ?? 0) + 1)
  }

  const colors = sliceColors.value
  return entries
    .sort((a, b) => b.cost - a.cost)
    .map((e, index) => {
      const raw = e.name.trim() || e.ref.trim() || '未知'
      const isDuplicate = (nameCounts.get(raw) ?? 0) > 1
      const uniqueName = isDuplicate && e.ref.trim() !== raw ? `${raw} (${e.ref.trim()})` : raw

      return {
        ref: e.ref,
        rawName: raw,
        name: uniqueName,
        value: Number(e.cost.toFixed(6)),
        requestCount: e.requests,
        currency: e.currency,
        costDisplay: formatCostValue(e.cost, e.currency),
        itemStyle: {
          color: colors[index % colors.length],
        },
      }
    })
})

const hasKeyData = computed(() => !props.loading && keyAccountSlices.value.length > 0)
const hasAccountData = computed(() => !props.loading && accountKeySlices.value.length > 0)
const tooltipContainers = new Map<string, HTMLElement>()

function createPieOption(slices: SliceItem[], seriesName: string): EChartsOption {
  const theme = palette.value
  const tooltipStyle = chartTooltipStyle(theme, { padding: [10, 14] })

  return {
    animationDuration: 240,
    tooltip: {
      trigger: 'item',
      ...tooltipStyle,
      renderMode: 'html',
      // 卡片和页面滚动容器会裁切后代；气泡挂到 body，并按整个视口而非 156px 的环图定位。
      appendTo: (container) => {
        tooltipContainers.set(seriesName, container)
        return document.body
      },
      className: 'quota-chart-tooltip',
      confine: false,
      transitionDuration: 0,
      extraCssText: `${tooltipStyle.extraCssText}box-sizing:border-box;max-width:min(360px,calc(100vw - 24px));white-space:normal;overflow-wrap:anywhere;`,
      position: (point, _params, _dom, _rect, size) => {
        const bounds = tooltipContainers.get(seriesName)?.getBoundingClientRect()
        if (!bounds)
          return point
        const scaleX = bounds.width / size.viewSize[0] || 1
        const scaleY = bounds.height / size.viewSize[1] || 1
        const pointerX = bounds.left + point[0] * scaleX
        const pointerY = bounds.top + point[1] * scaleY
        const [width, height] = size.contentSize
        const viewport = document.documentElement
        const x = pointerX + width + 16 <= viewport.clientWidth - 12 ? pointerX + 16 : pointerX - width - 16
        const y = pointerY + height + 16 <= viewport.clientHeight - 12 ? pointerY + 16 : pointerY - height - 16
        return [
          (Math.max(12, Math.min(x, viewport.clientWidth - width - 12)) - bounds.left) / scaleX,
          (Math.max(12, Math.min(y, viewport.clientHeight - height - 12)) - bounds.top) / scaleY,
        ]
      },
      formatter: (params: unknown) => {
        if (!isRecord(params))
          return ''
        const data = isRecord(params.data) ? params.data : {}
        const rawName = typeof data.rawName === 'string' && data.rawName ? data.rawName : (typeof params.name === 'string' ? params.name : '')
        const ref = typeof data.ref === 'string' ? data.ref : ''
        const title = ref && ref !== rawName ? `${rawName} (${ref})` : rawName
        const color = typeof params.color === 'string' ? params.color : theme.info
        const costDisplay = typeof data.costDisplay === 'string' ? data.costDisplay : '—'
        const percent = typeof params.percent === 'number' ? `${params.percent.toFixed(1)}%` : '—'
        const requestCount = typeof data.requestCount === 'number' ? formatCompactNumber(data.requestCount) : '—'

        return usageTooltipContent(theme, title, [
          usageTooltipItem('费用', costDisplay, color),
          usageTooltipItem('占比', percent, color),
          usageTooltipItem('请求数', `${requestCount} 次`, color),
        ])
      },
    },
    series: [
      {
        name: seriesName,
        type: 'pie',
        radius: ['68%', '88%'],
        center: ['50%', '50%'],
        itemStyle: {
          borderRadius: 3,
          borderColor: theme.surface,
          borderWidth: 1.5,
        },
        label: { show: false },
        labelLine: { show: false },
        emphasis: { scaleSize: 3 },
        data: slices,
      },
    ],
  }
}

const keyChartOption = computed<EChartsOption>(() =>
  createPieOption(keyAccountSlices.value, '账号消耗占比'),
)

const accountChartOption = computed<EChartsOption>(() =>
  createPieOption(accountKeySlices.value, '密钥来源占比'),
)

const emptyKeyText = computed(() => {
  if (props.items.length === 0)
    return '当前范围暂无配额消耗记录'
  if (!selectedKey.value)
    return '请选择 API 密钥'
  return '该 API 密钥在此时间段内无产生费用的账号消耗'
})

const emptyAccountText = computed(() => {
  if (props.items.length === 0)
    return '当前范围暂无配额消耗记录'
  if (!selectedAccount.value)
    return '请选择账号'
  return '该账号在此时间段内无产生费用的密钥消耗'
})

function legendItems(slices: SliceItem[]) {
  const total = slices.reduce((sum, slice) => sum + slice.value, 0)
  return slices.map(slice => ({
    ...slice,
    shareDisplay: formatPercent(total > 0 ? slice.value / total : null),
  }))
}

const panels = computed(() => [
  {
    id: 'key',
    title: '密钥 → 账号',
    description: '这把密钥的费用花在哪些账号',
    selectorLabel: '选择 API 密钥查看账号消耗分布',
    placeholder: '选择 API 密钥',
    selection: selectedKey.value,
    options: keyOptions.value,
    slices: legendItems(keyAccountSlices.value),
    countLabel: '使用账号',
    option: keyChartOption.value,
    hasData: hasKeyData.value,
    emptyText: emptyKeyText.value,
  },
  {
    id: 'account',
    title: '账号 → 密钥',
    description: '这个账号的费用来自哪些密钥',
    selectorLabel: '选择账号查看密钥消耗分布',
    placeholder: '选择账号',
    selection: selectedAccount.value,
    options: accountOptions.value,
    slices: legendItems(accountKeySlices.value),
    countLabel: '调用密钥',
    option: accountChartOption.value,
    hasData: hasAccountData.value,
    emptyText: emptyAccountText.value,
  },
])

function selectSource(panel: string, value: string) {
  if (panel === 'key')
    selectedKey.value = value
  else
    selectedAccount.value = value
}
</script>

<template>
  <div class="quota-charts">
    <section v-for="panel in panels" :key="panel.id" class="quota-chart-card rounded-cp-card" :aria-label="panel.title">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <h4 class="m-0 text-cp-sm font-heavy text-cp-text">
            {{ panel.title }}
          </h4>
          <p class="mt-1 mb-0 text-cp-xs text-cp-text-secondary">
            {{ panel.description }}
          </p>
        </div>
        <span class="shrink-0 rounded-full bg-cp-fill-tertiary px-2 py-1 text-cp-xs text-cp-text-secondary">费用占比</span>
      </div>

      <BaseSelect
        :model-value="panel.selection"
        :options="panel.options"
        :aria-label="panel.selectorLabel"
        :placeholder="panel.placeholder"
        :disabled="loading || panel.options.length === 0"
        size="sm"
        searchable
        class="w-full min-w-0"
        @update:model-value="selectSource(panel.id, $event)"
      />

      <div v-if="panel.hasData" class="quota-distribution">
        <div class="quota-ring" aria-hidden="true">
          <BaseChart :option="panel.option" :height="176" />
          <div class="quota-ring-center">
            <strong class="text-2xl font-heavy tabular-nums text-cp-text">{{ panel.slices.length }}</strong>
            <span class="text-cp-xs text-cp-text-secondary">{{ panel.countLabel }}</span>
          </div>
        </div>
        <BaseScrollbar height="176px" always-visible class="quota-legend">
          <ul class="m-0 list-none p-0 pr-3" :aria-label="`${panel.title}费用占比列表`">
            <li v-for="slice in panel.slices" :key="slice.ref" class="quota-legend-row">
              <span class="mt-1.5 size-2 shrink-0 rounded-full" :style="{ backgroundColor: slice.itemStyle.color }" />
              <div class="min-w-0">
                <span class="block truncate text-cp-xs font-emphasis text-cp-text" :title="slice.name">{{ slice.rawName }}</span>
                <span class="mt-0.5 block text-cp-xs tabular-nums text-cp-text-secondary">{{ slice.costDisplay }} · {{ formatCompactNumber(slice.requestCount) }} 次</span>
              </div>
              <span class="font-mono text-cp-xs tabular-nums text-cp-text">{{ slice.shareDisplay }}</span>
            </li>
          </ul>
        </BaseScrollbar>
      </div>
      <BaseEmpty
        v-else
        size="sm"
        surface="none"
        :title="loading ? '正在加载数据' : '暂无费用分布'"
        :description="panel.emptyText"
        class="h-44 place-content-center"
      />
    </section>
  </div>
</template>

<style scoped>
.quota-charts {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(min(100%, 440px), 1fr));
  gap: 16px;
  min-width: 0;
}

.quota-chart-card {
  display: grid;
  min-width: 0;
  align-content: start;
  gap: 12px;
  padding: 16px;
  border: 1px solid var(--cp-color-split);
}

.quota-distribution {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 16px;
  min-width: 0;
}

.quota-ring {
  position: relative;
  flex: 0 0 156px;
  min-width: 0;
  margin-inline: auto;
}

.quota-legend {
  flex: 1 1 180px;
  min-width: 0;
}

.quota-ring-center {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 2px;
  pointer-events: none;
}

.quota-legend-row {
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto;
  align-items: start;
  gap: 8px;
  padding: 10px 0;
}

.quota-legend-row + .quota-legend-row {
  border-top: 1px solid var(--cp-color-split);
}
</style>

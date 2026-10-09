<script setup lang="ts">
import type { UsageKeyAccountBreakdownItem, UsageKeyAccountBreakdownResponse } from '@/api'
import type { SelectOption } from '@/components/base/BaseSelect.vue'

import { formatPercent, formatUsd } from '@kit/format'
import { RefreshCw } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import CatalogFilter from '@/components/CatalogFilter.vue'
import { formatLocalizedCompactNumber as formatCompactNumber } from '@/utils/number'
import KeyAccountQuotaCharts from './KeyAccountQuotaCharts.vue'

const props = withDefaults(
  defineProps<{
    breakdown: UsageKeyAccountBreakdownResponse
    loading?: boolean
    refreshing?: boolean
  }>(),
  {
    loading: false,
    refreshing: false,
  },
)

const emit = defineEmits<{
  refresh: []
}>()

const search = defineModel<string>('search', { default: '' })
const catalogOptions = shallowRef<{ accounts: SelectOption[], keys: SelectOption[] }>({ accounts: [], keys: [] })

watch(() => props.breakdown.items, (items) => {
  // 保留本页已见过的身份选项，筛选后的子集不能让其它账号或密钥从下拉框消失。
  const accounts = new Map(catalogOptions.value.accounts.map(option => [option.value, option]))
  const keys = new Map(catalogOptions.value.keys.map(option => [option.value, option]))
  for (const item of items) {
    accounts.set(item.providerAccountRef, {
      value: item.providerAccountRef,
      label: item.providerAccountName || item.providerAccountRef,
      description: item.providerAccountRef,
    })
    keys.set(item.clientApiKeyRef, {
      value: item.clientApiKeyRef,
      label: item.clientApiKeyName || item.clientApiKeyRef,
      description: item.clientApiKeyRef,
    })
  }
  catalogOptions.value = { accounts: [...accounts.values()], keys: [...keys.values()] }
}, { immediate: true })

const columns = defineTableColumns<KeyAccountDisplayItem>([
  {
    key: 'keyName',
    label: '密钥',
    kind: 'custom',
    size: 'xl',
  },
  {
    key: 'accountName',
    label: '账号',
    kind: 'custom',
    size: 'xl',
  },
  {
    key: 'requestCount',
    label: '请求',
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'successRate',
    label: '成功率',
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'totalTokens',
    label: '总 Token',
    kind: 'numeric',
    size: 'sm',
  },
  {
    key: 'cost',
    label: '费用',
    kind: 'numeric',
    size: 'sm',
  },
])

type KeyAccountDisplayItem = UsageKeyAccountBreakdownItem & {
  rowKey: string
  keyNameDisplay: { primary: string, secondary: string, full: string }
  accountNameDisplay: { primary: string, secondary: string, full: string }
  successRate: number | null
  costDisplay: string
}

const displayItems = computed<KeyAccountDisplayItem[]>(() =>
  props.breakdown.items.map(item => ({
    ...item,
    rowKey: `${item.clientApiKeyRef}::${item.providerAccountRef}`,
    keyNameDisplay: pairName(item.clientApiKeyName, item.clientApiKeyRef),
    accountNameDisplay: pairName(item.providerAccountName, item.providerAccountRef),
    successRate: item.requestCount > 0 ? item.successCount / item.requestCount : null,
    costDisplay: costText(item.costAmount, item.costCurrency),
  })),
)

function pairName(name: string, ref: string) {
  const primary = name.trim() || ref.trim() || '未知'
  const secondary = primary === ref ? '' : ref
  return {
    primary,
    secondary,
    full: secondary ? `${primary} → ${ref}` : primary,
  }
}

function costText(amount: string | null, currency: string | null) {
  if (amount == null)
    return '—'
  if (currency && currency !== 'USD')
    return `${currency} ${amount}`
  return formatUsd(amount)
}
</script>

<template>
  <div class="quota-breakdown">
    <div
      class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"
      role="group"
      aria-label="配额消耗筛选与操作"
    >
      <div class="min-w-0">
        <h3 class="m-0 text-base leading-snug font-heavy text-cp-text">
          配额消耗
        </h3>
        <p class="mt-0.5 mb-0 text-cp-xs leading-normal font-emphasis text-cp-text-secondary">
          查看密钥与账号之间的用量，跟随页面顶部的时间范围
        </p>
      </div>

      <div class="flex min-w-0 items-center gap-2 sm:shrink-0">
        <CatalogFilter
          v-model="search"
          kind="quota"
          label="按密钥或账号筛选"
          :supplemental-options="catalogOptions"
          class="min-w-0 flex-1 sm:w-100"
        />
        <BaseIconButton
          variant="ghost"
          size="md"
          label="刷新配额消耗"
          :loading="refreshing"
          :disabled="loading || refreshing"
          @click="emit('refresh')"
        >
          <template #loading>
            <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-4.5" />
        </BaseIconButton>
      </div>
    </div>
    <KeyAccountQuotaCharts
      :items="breakdown.items"
      :loading="loading"
    />

    <section class="min-w-0" aria-label="配额消耗明细">
      <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h4 class="m-0 text-cp-sm font-heavy text-cp-text">
          消耗明细 <span class="ml-1 font-normal text-cp-text-tertiary">{{ displayItems.length }} 组</span>
        </h4>
        <span class="text-cp-xs text-cp-text-tertiary">每行对应一个密钥与账号组合</span>
      </div>
      <!-- 明细有独立高度，不能与图表竞争剩余空间，否则 size containment 会将表格压成零高。 -->
      <div class="quota-detail-table">
        <BaseTable
          class="h-full"
          :columns="columns"
          :rows="displayItems"
          :loading="loading"
          density="compact"
          row-key="rowKey"
          empty-text="暂无配额消耗数据"
          horizontal-controls
          scrollbar-always-visible
        >
          <template #keyName="{ row }">
            <div class="inline-grid max-w-full min-w-0 gap-1" :title="row.keyNameDisplay.full">
              <span
                class="block max-w-full truncate text-cp-sm leading-normal font-emphasis text-cp-text"
              >
                {{ row.keyNameDisplay.primary }}
              </span>
              <code
                v-if="row.keyNameDisplay.secondary"
                class="block max-w-full truncate font-mono text-cp-xs text-cp-text-quaternary"
              >
                {{ row.keyNameDisplay.secondary }}
              </code>
            </div>
          </template>

          <template #accountName="{ row }">
            <div class="inline-grid max-w-full min-w-0 gap-1" :title="row.accountNameDisplay.full">
              <span class="block max-w-full truncate text-cp-sm leading-normal font-emphasis text-cp-text">
                {{ row.accountNameDisplay.primary }}
              </span>
              <code
                v-if="row.accountNameDisplay.secondary"
                class="block max-w-full truncate font-mono text-cp-xs text-cp-text-quaternary"
              >
                {{ row.accountNameDisplay.secondary }}
              </code>
            </div>
          </template>

          <template #requestCount="{ row }">
            <span class="font-mono font-bold tabular-nums text-cp-text">
              {{ formatCompactNumber(row.requestCount) }}
            </span>
          </template>

          <template #successRate="{ row }">
            <strong
              class="font-mono font-bold tabular-nums"
              :class="row.failureCount > 0 ? 'text-cp-text-secondary' : 'text-cp-green-text'"
              :title="`成功 ${formatCompactNumber(row.successCount)}，失败 ${formatCompactNumber(row.failureCount)}`"
            >
              {{ formatPercent(row.successRate) }}
            </strong>
          </template>

          <template #totalTokens="{ row }">
            <span class="font-mono font-bold tabular-nums text-cp-text">
              {{ formatCompactNumber(row.totalTokens) }}
            </span>
          </template>

          <template #cost="{ row }">
            <span class="font-mono font-bold tabular-nums text-cp-text">
              {{ row.costDisplay }}
            </span>
          </template>

          <template #empty>
            <BaseEmpty
              size="sm"
              surface="none"
              :title="loading ? '正在加载配额消耗数据' : '暂无配额消耗数据'"
              :description="search ? '未找到匹配该密钥或账号的配额消耗记录' : '当前范围没有按密钥 × 账号统计的请求记录'"
              class="h-full place-content-center"
            />
          </template>
        </BaseTable>
      </div>
    </section>
  </div>
</template>

<style scoped>
.quota-breakdown {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 20px;
}

.quota-detail-table {
  height: clamp(320px, 50vh, 480px);
  min-width: 0;
}
</style>

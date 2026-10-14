<script setup lang="ts">
import type { PoolStatistics, UsageBatch, UsageRecord } from '../history'
import type { DirectoryAccount, Observation } from '../model'
import { usePluginPolling } from '@sdk/polling'
import { call } from '@sdk/ui'
import { computed, nextTick, onActivated, onDeactivated, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { batchLabels, reasonLabels, historyTime as time } from '../history'
import { duration, sourceLabel } from '../model'
import PoolCoolingSummary from './PoolCoolingSummary.vue'
import PoolUsageCharts from './PoolUsageCharts.vue'

const props = defineProps<{ accounts: DirectoryAccount[], mode: 'records' | 'summary' }>()
// 备用票从未转正的记录没有窗口；已转正的窗口从第一次业务访问算起。
function stateText(row: UsageRecord) {
  if (row.verifiedAt == null)
    return row.probeEndedAt == null ? '检测中' : '未形成窗口'
  if (row.standby && row.activatedAt == null)
    return row.retiredAt == null ? '备用中' : '备用票已丢弃'
  return row.retiredAt == null ? '使用中' : '已结束'
}
const emit = defineEmits<{ records: [] }>()
const data = shallowRef<PoolStatistics>()
const error = shallowRef('')
const loading = shallowRef(false)
const active = shallowRef(true)
const tab = computed(() => props.mode)
const range = shallowRef('24')
const from = shallowRef('')
const to = shallowRef('')
const account = shallowRef('')
const gateway = shallowRef('')
const batch = shallowRef('')
const page = shallowRef(1)
const pageSize = shallowRef(20)
const expanded = shallowRef<(string | number)[]>([])
const frozenTo = shallowRef<number>()
const names = computed(() => new Map(props.accounts.map(item => [item.id, item.name || item.email || item.id])))
const accountOptions = computed(() => [
  { label: tab.value === 'summary' ? '选择账号' : '全部账号', value: '' },
  ...[...new Set([...props.accounts.map(item => item.id), ...(data.value?.options.accounts ?? [])])].map(id => ({ label: names.value.get(id) ?? id, value: id })),
])
const gatewayOptions = computed(() => [{ label: '全部网关', value: '' }, ...(data.value?.options.gateways ?? []).map(value => ({ label: value, value }))])
const batchOptions = computed(() => [{ label: '全部批次', value: '' }, ...(data.value?.options.batches ?? []).map(item => ({ label: `${names.value.get(item.accountId) ?? item.accountId} · 第 ${item.number} 批`, value: item.id }))])
const pagination = computed(() => ({ currentPage: data.value?.page.number ?? page.value, pageSize: pageSize.value, total: data.value?.page.total ?? 0 }))
const formatDuration = (value: unknown) => duration(typeof value === 'number' ? value : null)
const recordColumns = defineTableColumns<UsageRecord>([
  { key: 'accountId', label: '账号', kind: 'identity', size: 'xl' },
  { key: 'gateway', label: '网关 / 批次', kind: 'custom', size: 'lg' },
  { key: 'period', label: '窗口起止', kind: 'custom', size: 'xl' },
  { key: 'windowSeconds', label: '窗口时长', kind: 'numeric', format: formatDuration },
  { key: 'state', label: '状态', kind: 'custom', size: 'lg' },
  { key: 'actions', label: '详情', kind: 'actions' },
])
const observationColumns = defineTableColumns<Observation>([
  { key: 'at', label: '时间', kind: 'datetime', format: value => time(typeof value === 'number' ? value : null) },
  { key: 'source', label: '类型', kind: 'text', size: 'lg', format: (_value, row) => sourceLabel(row) },
  { key: 'model', label: '模型', kind: 'mono', format: value => value ?? '—' },
  { key: 'classification', label: '结果', kind: 'text', size: 'lg', format: (value, row) => value === '21' ? '21 满血' : value === '29' ? '29 降智' : row.status == null ? '未响应' : row.status !== 200 ? `失败 ${row.status}` : '不判定' },
  { key: 'stateReturned', label: '返回 state', kind: 'text', size: 'sm', format: (value, row) => row.status === 200 ? value ? '是' : '否' : '—' },
])
const batchColumns = defineTableColumns<UsageBatch>([
  { key: 'number', label: '批次', kind: 'custom', size: 'lg' },
  { key: 'coverage', label: '已使用网关', kind: 'custom', size: 'lg' },
  { key: 'windowSeconds', label: '窗口总时长', kind: 'numeric', format: formatDuration },
  { key: 'status', label: '状态', kind: 'custom', size: 'lg' },
  { key: 'actions', label: '查看', kind: 'actions' },
])
let revision = 0
async function refresh(background = false) {
  const current = ++revision
  if (tab.value === 'summary' && !account.value) {
    data.value = undefined
    error.value = ''
    loading.value = false
    return
  }
  loading.value = !background
  try {
    const end = range.value === 'custom' ? new Date(to.value).getTime() / 1000 : frozenTo.value ?? Date.now() / 1000
    const start = range.value === 'all' ? null : range.value === 'custom' ? new Date(from.value).getTime() / 1000 : end - Number(range.value) * 3600
    if (!Number.isFinite(end) || (start != null && !Number.isFinite(start)))
      throw new Error('请选择完整的起止时间')
    const response = await call<PoolStatistics>('gateway-pool.history.read', {
      view: tab.value,
      from: start,
      to: end,
      accountId: account.value,
      gateway: gateway.value,
      batchId: batch.value,
      page: page.value,
      pageSize: pageSize.value,
    })
    if (current !== revision)
      return
    if (response.version !== 2)
      throw new Error('当前控制面缺少新版计时支持')
    data.value = response
    error.value = ''
  }
  catch (cause) {
    if (current === revision) {
      error.value = cause instanceof Error ? cause.message : '读取轮转统计失败'
      data.value = undefined
    }
  }
  finally {
    if (current === revision)
      loading.value = false
  }
}
function toggle(id: string | number) {
  expanded.value = expanded.value.includes(id) ? [] : [id]
}
function reset() {
  account.value = ''
  gateway.value = ''
  batch.value = ''
}
function drillGateway(value: string) {
  gateway.value = value
  batch.value = ''
  emit('records')
}
// 最近 24 小时图里点某个账号的网关：先切换账号（会清空网关筛选），再设网关。
async function drillWindow(value: { accountId: string, gateway: string }) {
  if (account.value !== value.accountId) {
    account.value = value.accountId
    await nextTick()
  }
  drillGateway(value.gateway)
}
function drillBatch(value: string) {
  batch.value = value
  gateway.value = ''
  emit('records')
}
function changePage(value: number) {
  expanded.value = []
  frozenTo.value = value === 1 ? undefined : data.value?.to
  page.value = value
  void refresh()
}
watch(account, () => {
  batch.value = ''
  gateway.value = ''
})
watch(tab, (value) => {
  if (value === 'summary') {
    gateway.value = ''
    batch.value = ''
    if (!account.value)
      account.value = accountOptions.value[1]?.value ?? ''
  }
}, { immediate: true })
watch([tab, account, gateway, batch, range, from, to, pageSize], () => {
  expanded.value = []
  page.value = 1
  frozenTo.value = undefined
  void refresh()
})
onMounted(() => {
  void refresh()
})
onActivated(() => {
  active.value = true
})
onDeactivated(() => {
  active.value = false
  revision++
  loading.value = false
})
onScopeDispose(() => {
  revision++
})
usePluginPolling(() => refresh(true), computed(() => active.value ? 10_000 : 0))
</script>

<template>
  <div class="flex min-w-0 flex-col gap-4">
    <div class="flex flex-wrap items-center gap-2">
      <BaseSelect v-model="range" :options="[{ label: '最近 24 小时', value: '24' }, { label: '最近 7 天', value: '168' }, { label: '全部已采集', value: 'all' }, { label: '自定义时间', value: 'custom' }]" aria-label="时间范围" class="w-40" />
      <template v-if="range === 'custom'">
        <BaseInput v-model="from" type="datetime-local" aria-label="开始时间" class="w-56" />
        <BaseInput v-model="to" type="datetime-local" aria-label="结束时间" class="w-56" />
      </template>
      <BaseSelect v-model="account" :options="accountOptions" aria-label="账号" searchable class="w-48" />
      <BaseSelect v-if="tab !== 'summary'" v-model="gateway" :options="gatewayOptions" aria-label="网关" searchable class="w-40" />
      <BaseSelect v-if="tab === 'records'" v-model="batch" :options="batchOptions" aria-label="批次" searchable class="w-52" />
      <BaseButton size="sm" variant="ghost" @click="reset">
        重置
      </BaseButton>
      <BaseButton size="sm" :loading="loading" @click="refresh()">
        刷新
      </BaseButton>
    </div>
    <p v-if="error" role="alert" class="text-cp-sm text-cp-error-text">
      {{ error }}
    </p>
    <div v-if="data" class="flex flex-wrap items-center justify-between gap-2 text-cp-sm">
      <span class="font-emphasis">窗口总时长 {{ duration(data.summary.windowSeconds) }}</span>
      <span class="text-cp-xs text-cp-text-secondary">窗口时长计至最后一次确认可用；使用中的窗口计至当前，备用票转正的窗口从第一次业务访问算起</span>
    </div>
    <p v-if="data && (data.collectionGaps || data.summary.missing.windowSeconds)" class="text-cp-xs text-cp-warning-text">
      部分窗口记录缺失，合计仅包含已知时长
    </p>
    <template v-if="tab === 'summary'">
      <PoolCoolingSummary v-if="data?.coolingAnalysis" :data="data.coolingAnalysis" />
      <PoolUsageCharts v-if="data" :gateways="data.gateways" :recent="data.recentWindows ?? []" :names="names" @gateway="drillGateway" @window="drillWindow" />
      <BaseTable :columns="batchColumns" :rows="data?.batches ?? []" :loading="loading" empty-text="选择账号查看每批轮转">
        <template #number="{ row }">
          第 {{ row.number }} 批
        </template>
        <template #coverage="{ row }">
          {{ row.served }} / {{ row.candidates.length }}
        </template>
        <template #status="{ row }">
          {{ batchLabels[row.status] }}<span v-if="row.partial || row.missing.windowSeconds" class="block text-cp-xs text-cp-text-secondary">{{ row.partial ? '部分计入范围' : '含缺失计时' }}</span>
        </template>
        <template #actions="{ row }">
          <BaseButton size="sm" variant="ghost" @click="drillBatch(row.id)">
            轮转明细
          </BaseButton>
        </template>
      </BaseTable>
    </template>
    <BaseTable v-else-if="tab === 'records'" :columns="recordColumns" :rows="data?.records ?? []" :loading="loading" :expanded-row-keys="expanded" empty-text="当前范围没有轮转记录">
      <template #accountId="{ row }">
        {{ names.get(row.accountId) ?? row.accountId }}
      </template>
      <template #gateway="{ row }">
        <span class="font-mono">{{ row.gateway }}</span><span class="block text-cp-xs text-cp-text-secondary">第 {{ row.batchNumber }} 批</span>
      </template>
      <template #period="{ row }">
        <span v-if="row.verifiedAt != null && row.windowStartAt != null" class="text-cp-xs text-cp-text-secondary">{{ time(row.windowStartAt) }}<br>{{ row.windowEndAt != null ? time(row.windowEndAt) : row.retiredAt == null ? '使用中' : time(row.retiredAt) }}</span>
        <span v-else class="text-cp-xs text-cp-text-secondary">—</span>
      </template>
      <template #state="{ row }">
        {{ stateText(row) }}<span v-if="row.partial" class="block text-cp-xs text-cp-text-secondary">部分计入范围</span>
      </template>
      <template #actions="{ row }">
        <BaseButton size="sm" variant="ghost" :aria-expanded="expanded.includes(row.id)" @click="toggle(row.id)">
          {{ expanded.includes(row.id) ? '收起' : '详情' }}
        </BaseButton>
      </template>
      <template #expanded="{ row }">
        <div class="space-y-3 py-3">
          <div class="flex flex-wrap gap-x-5 gap-y-2 text-cp-xs text-cp-text-secondary">
            <span v-if="row.standby">备用票拿到 {{ time(row.standbyAt) }}</span>
            <span v-if="row.standby">启用 {{ time(row.activatedAt) }}</span>
            <span>验证可用 {{ time(row.verifiedAt) }}</span>
            <span>最后确认可用 {{ time(row.lastConfirmedAt) }}</span>
            <span>窗口时长 {{ duration(row.windowSeconds) }}</span>
            <span>结束原因 {{ row.reason ? reasonLabels[row.reason] ?? row.reason : '—' }}</span>
            <span>检测结果 {{ row.probeResult === '21' ? '21 满血' : row.probeResult === '29' ? '29 降智' : row.probeResult === 'failed' ? '未通过' : '待结果' }}</span>
          </div>
          <h4 class="text-cp-sm font-emphasis">
            检测与访问记录 <span class="text-cp-xs font-normal text-cp-text-secondary">{{ row.observationCount }} 条{{ row.observationCount > row.observations.length ? ' · 展示最近 100 条' : '' }}</span>
          </h4>
          <BaseTable :columns="observationColumns" :rows="row.observations" density="compact" empty-text="此轮暂无关联记录，旧数据可能缺少轮转标识" />
        </div>
      </template>
    </BaseTable>
    <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="changePage" @page-size-change="pageSize = $event" />
  </div>
</template>

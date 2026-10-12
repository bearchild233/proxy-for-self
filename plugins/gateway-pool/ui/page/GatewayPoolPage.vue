<script setup lang="ts">
import type { DirectoryAccount, Observation, PoolView } from './model'
import { Activity, ArrowRight, Hourglass, Network, Settings2, Shuffle } from '@lucide/vue'
import { usePluginPolling } from '@sdk/polling'
import { call } from '@sdk/ui'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, ref, shallowRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { toast } from '@/components/base/BaseToast'
import { formatDateTime } from '@/utils/date'
import AccountPoolCard from './components/AccountPoolCard.vue'
import PoolSettingsModal from './components/PoolSettingsModal.vue'
import { duration, sourceLabel } from './model'

const view = ref<PoolView>()
const groups = ref<{ id: string, name: string }[]>([])
const accounts = ref<DirectoryAccount[]>([])
const error = shallowRef('')
const saving = shallowRef(false)
const settingsOpen = shallowRef(false)
const tab = shallowRef('accounts')
const accountFilter = shallowRef('')
const gatewayFilter = shallowRef('')
const now = shallowRef(Date.now() / 1000)
useIntervalFn(() => {
  now.value = Date.now() / 1000
}, 1000)

const names = computed(() => new Map(accounts.value.map(account => [account.id, account.name || account.email || account.id])))
const settings = computed(() => view.value?.settings)
const enabledGroups = computed(() => groups.value.filter(group => settings.value?.groupIds.includes(group.id)))
// 账号卡片：选定分组的成员，加上已有组合记录但已离开分组的账号。
const accountCards = computed(() => {
  const pairs = view.value?.pairs ?? []
  const ids = new Set(accounts.value.filter(account => account.groups?.some(group => settings.value?.groupIds.includes(group.id))).map(account => account.id))
  pairs.forEach(pair => ids.add(pair.accountId))
  return [...ids]
    .map(id => ({ id, name: names.value.get(id) ?? id, pairs: pairs.filter(pair => pair.accountId === id) }))
    .sort((a, b) => Number(b.pairs.some(pair => pair.state === 'ready')) - Number(a.pairs.some(pair => pair.state === 'ready')) || a.name.localeCompare(b.name))
})
const hourAgo = computed(() => now.value - 3600)
const recent = computed(() => (view.value?.observations ?? []).filter(sample => sample.at >= hourAgo.value))
const stats = computed(() => {
  const pairs = view.value?.pairs ?? []
  const switches = (view.value?.switches ?? []).filter(item => item.at >= hourAgo.value)
  const windows = switches.map(item => item.previousWindowSeconds).filter((value): value is number => value != null)
  return {
    serving: pairs.filter(pair => pair.state === 'ready').length,
    accounts: accountCards.value.length,
    cooling: pairs.filter(pair => pair.state === 'cooling' && (pair.restUntil ?? 0) > now.value).length,
    pairs: pairs.length,
    normal: recent.value.filter(sample => sample.classification === '21').length,
    degraded: recent.value.filter(sample => sample.classification === '29').length,
    failed: recent.value.filter(sample => sample.source !== 'probe-attempt' && sample.classification == null && sample.status != null && sample.status !== 200).length,
    switches: switches.length,
    averageWindow: windows.length ? windows.reduce((sum, value) => sum + value, 0) / windows.length : null,
  }
})
const tabOptions = [
  { label: '账号与网关', value: 'accounts' },
  { label: '检测记录', value: 'observations' },
  { label: '切换记录', value: 'switches' },
]
const accountOptions = computed(() => [{ label: '全部账号', value: '' }, ...accountCards.value.map(card => ({ label: card.name, value: card.id }))])
const gatewayOptions = computed(() => [{ label: '全部网关', value: '' }, ...(settings.value?.gateways ?? []).map(gateway => ({ label: gateway, value: gateway }))])
const observations = computed(() => (view.value?.observations ?? []).filter(sample =>
  (!accountFilter.value || sample.account === accountFilter.value) && (!gatewayFilter.value || sample.gateway === gatewayFilter.value)))
const switches = computed(() => (view.value?.switches ?? []).filter(item =>
  (!accountFilter.value || item.accountId === accountFilter.value) && (!gatewayFilter.value || item.toGateway === gatewayFilter.value || item.fromGateway === gatewayFilter.value)))

function verdict(sample: Observation): { text: string, type: 'success' | 'danger' | 'warning' | 'neutral' } {
  if (sample.classification === '21')
    return { text: '21 满血', type: 'success' }
  if (sample.classification === '29')
    return { text: '29 降智', type: 'danger' }
  if (sample.status != null && sample.status !== 200)
    return { text: `失败 ${sample.status}`, type: 'warning' }
  if (sample.status == null && sample.source !== 'probe-attempt')
    return { text: '未响应', type: 'warning' }
  return { text: '不判定', type: 'neutral' }
}
const time = (seconds: number) => formatDateTime(seconds * 1000)

async function refresh() {
  try {
    view.value = await call<PoolView>('gateway-pool.read', { limit: 200 })
    error.value = ''
  }
  catch (e) { error.value = e instanceof Error ? e.message : '读取网关池失败' }
}
async function loadDirectory() {
  // 分页读取完整目录，不把首屏当作全部账号与分组。
  function unpack<T>(result: { data?: T } | T): T {
    return (result && typeof result === 'object' && 'data' in result ? result.data : result) as T
  }
  async function collect<T>(operation: string) {
    const items: T[] = []
    for (let page = 1; ; page++) {
      const result = unpack(await call<{ items: T[], page: { totalPages: number } }>(operation, { page, pageSize: 100 }))
      items.push(...result.items)
      if (page >= result.page.totalPages)
        return items
    }
  }
  try {
    [groups.value, accounts.value] = await Promise.all([
      collect<{ id: string, name: string }>('account-groups.get.admin.account-groups'),
      collect<DirectoryAccount>('accounts.get.admin.accounts'),
    ])
  }
  catch { error.value = '账号或分组目录读取失败' }
}
async function save(payload: Record<string, unknown>) {
  if (saving.value)
    return
  saving.value = true
  try {
    await call('gateway-pool.configure', payload)
    settingsOpen.value = false
    toast.success('网关池设置已保存')
    await refresh()
  }
  catch (e) { toast.error(e instanceof Error ? e.message : '保存失败') }
  finally { saving.value = false }
}
onMounted(() => {
  void refresh()
  void loadDirectory()
})
usePluginPolling(refresh, shallowRef(10_000))
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col gap-5 overflow-auto pb-6">
    <BasePageHeader class="h-17" title="网关池" description="为降智分组按账号×网关轮换满血网关，降智信号对用户不可见">
      <template #actions>
        <div class="flex items-center gap-2">
          <BaseTag v-if="view?.preview" type="warning" size="lg">
            模拟数据预览
          </BaseTag>
          <BaseTag :type="enabledGroups.length ? 'success' : 'neutral'" size="lg" round>
            {{ enabledGroups.length ? `已用于 ${enabledGroups.map(group => group.name).join('、')}` : '未选择分组' }}
          </BaseTag>
          <BaseButton :disabled="!settings" @click="settingsOpen = true">
            <template #icon>
              <Settings2 class="size-4" />
            </template>
            设置
          </BaseButton>
        </div>
      </template>
    </BasePageHeader>

    <p v-if="error" role="alert" class="rounded-cp border border-cp-error-border bg-cp-error-container px-4 py-2.5 text-cp-sm text-cp-error-on-container">
      {{ error }}
    </p>

    <div class="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      <div class="rounded-cp-lg border border-cp-border bg-cp-bg-container p-4">
        <div class="flex items-center gap-2 text-cp-xs text-cp-text-secondary">
          <Network class="size-4 text-cp-success" />正在服务
        </div>
        <div class="mt-2 text-2xl font-emphasis tabular-nums text-cp-text">
          {{ stats.serving }}<span class="ml-1 text-cp-sm text-cp-text-tertiary">/ {{ stats.accounts }} 个账号</span>
        </div>
      </div>
      <div class="rounded-cp-lg border border-cp-border bg-cp-bg-container p-4">
        <div class="flex items-center gap-2 text-cp-xs text-cp-text-secondary">
          <Hourglass class="size-4 text-cp-text-tertiary" />静置中的组合
        </div>
        <div class="mt-2 text-2xl font-emphasis tabular-nums text-cp-text">
          {{ stats.cooling }}<span class="ml-1 text-cp-sm text-cp-text-tertiary">/ {{ stats.pairs }} 个账号×网关组合</span>
        </div>
      </div>
      <div class="rounded-cp-lg border border-cp-border bg-cp-bg-container p-4">
        <div class="flex items-center gap-2 text-cp-xs text-cp-text-secondary">
          <Activity class="size-4 text-cp-info" />近 1 小时判定
        </div>
        <div class="mt-2 flex items-baseline gap-3 text-2xl font-emphasis tabular-nums">
          <span class="text-cp-success-text" title="21 满血">{{ stats.normal }}</span>
          <span class="text-cp-error-text" title="29 降智">{{ stats.degraded }}</span>
          <span class="text-cp-warning-text" title="检测失败">{{ stats.failed }}</span>
          <span class="text-cp-xs font-normal text-cp-text-tertiary">满血 · 降智 · 失败</span>
        </div>
      </div>
      <div class="rounded-cp-lg border border-cp-border bg-cp-bg-container p-4">
        <div class="flex items-center gap-2 text-cp-xs text-cp-text-secondary">
          <Shuffle class="size-4 text-cp-primary-text" />近 1 小时切换
        </div>
        <div class="mt-2 text-2xl font-emphasis tabular-nums text-cp-text">
          {{ stats.switches }}<span class="ml-1 text-cp-sm text-cp-text-tertiary">次 · 平均窗口 {{ duration(stats.averageWindow) }}</span>
        </div>
      </div>
    </div>

    <BaseCard>
      <template #header>
        <div class="flex w-full flex-col gap-3 lg:flex-row lg:items-center">
          <BaseSegmented v-model="tab" label="网关池视图" :options="tabOptions" class="lg:w-96" />
          <div v-if="tab !== 'accounts'" class="flex gap-2 lg:ml-auto">
            <BaseSelect v-model="accountFilter" :options="accountOptions" aria-label="筛选账号" class="w-44" searchable />
            <BaseSelect v-model="gatewayFilter" :options="gatewayOptions" aria-label="筛选网关" class="w-40" searchable />
          </div>
        </div>
      </template>
      <template #body>
        <div v-if="tab === 'accounts'" class="grid gap-2.5">
          <AccountPoolCard
            v-for="card in accountCards"
            :key="card.id"
            :name="card.name"
            :pairs="card.pairs"
            :window-seconds="settings?.windowSeconds ?? 180"
            :probe-model="settings?.probeModel ?? 'gpt-6.1-sol'"
            :now="now"
          />
          <BaseEmpty
            v-if="!accountCards.length"
            title="还没有账号使用网关池"
            description="在设置中选择降智专用分组，有请求进入后会自动找票。"
          >
            <template #action>
              <BaseButton variant="primary" @click="settingsOpen = true">
                打开设置
              </BaseButton>
            </template>
          </BaseEmpty>
        </div>

        <div v-else-if="tab === 'observations'" class="overflow-auto">
          <table class="w-full border-collapse whitespace-nowrap text-cp-sm">
            <thead class="text-left text-cp-xs text-cp-text-secondary">
              <tr class="border-b border-cp-border">
                <th class="px-3 py-2.5 font-normal">
                  时间
                </th><th class="px-3 py-2.5 font-normal">
                  账号
                </th><th class="px-3 py-2.5 font-normal">
                  网关
                </th><th class="px-3 py-2.5 font-normal">
                  类型
                </th><th class="px-3 py-2.5 font-normal">
                  模型
                </th><th class="px-3 py-2.5 font-normal">
                  结果
                </th><th class="px-3 py-2.5 font-normal">
                  返回 state
                </th><th class="px-3 py-2.5 text-right font-normal">
                  耗时
                </th><th class="px-3 py-2.5 text-right font-normal">
                  检测前静置
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="sample in observations" :key="sample.id" class="border-b border-cp-border-secondary">
                <td class="px-3 py-2 tabular-nums text-cp-text-secondary">
                  {{ time(sample.at) }}
                </td>
                <td class="px-3 py-2 text-cp-text">
                  {{ names.get(sample.account) ?? sample.account }}
                </td>
                <td class="px-3 py-2 font-mono text-cp-xs">
                  {{ sample.gateway }}
                </td>
                <td class="px-3 py-2 text-cp-text-secondary">
                  {{ sourceLabel(sample) }}
                </td>
                <td class="px-3 py-2 font-mono text-cp-xs text-cp-text-secondary">
                  {{ sample.model ?? '—' }}
                </td>
                <td class="px-3 py-2">
                  <BaseTag size="sm" :type="verdict(sample).type">
                    {{ verdict(sample).text }}
                  </BaseTag>
                </td>
                <td class="px-3 py-2 text-cp-text-secondary">
                  {{ sample.status === 200 ? (sample.stateReturned ? '是' : '否') : '—' }}
                </td>
                <td class="px-3 py-2 text-right tabular-nums text-cp-text-secondary">
                  {{ sample.durationMs == null ? '—' : `${sample.durationMs} ms` }}
                </td>
                <td class="px-3 py-2 text-right tabular-nums text-cp-text-secondary">
                  {{ duration(sample.observedIdleSeconds) }}
                </td>
              </tr>
            </tbody>
          </table>
          <BaseEmpty v-if="!observations.length" size="sm" title="暂无检测记录" />
        </div>

        <div v-else class="overflow-auto">
          <table class="w-full border-collapse whitespace-nowrap text-cp-sm">
            <thead class="text-left text-cp-xs text-cp-text-secondary">
              <tr class="border-b border-cp-border">
                <th class="px-3 py-2.5 font-normal">
                  时间
                </th><th class="px-3 py-2.5 font-normal">
                  账号
                </th><th class="px-3 py-2.5 font-normal">
                  切换
                </th><th class="px-3 py-2.5 text-right font-normal">
                  旧网关服务时长
                </th><th class="px-3 py-2.5 text-right font-normal">
                  找票耗时
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(item, index) in switches" :key="`${item.at}-${index}`" class="border-b border-cp-border-secondary">
                <td class="px-3 py-2 tabular-nums text-cp-text-secondary">
                  {{ time(item.at) }}
                </td>
                <td class="px-3 py-2 text-cp-text">
                  {{ names.get(item.accountId) ?? item.accountId }}
                </td>
                <td class="px-3 py-2">
                  <span class="flex items-center gap-2 font-mono text-cp-xs">
                    <span :class="item.fromGateway ? 'text-cp-text-secondary' : 'text-cp-text-tertiary'">{{ item.fromGateway ?? '首次' }}</span>
                    <ArrowRight class="size-3.5 text-cp-text-tertiary" aria-hidden="true" />
                    <span class="text-cp-success-text">{{ item.toGateway }}</span>
                  </span>
                </td>
                <td class="px-3 py-2 text-right tabular-nums text-cp-text-secondary">
                  {{ duration(item.previousWindowSeconds) }}
                </td>
                <td class="px-3 py-2 text-right tabular-nums text-cp-text-secondary">
                  {{ duration(item.preparationSeconds) }}
                </td>
              </tr>
            </tbody>
          </table>
          <BaseEmpty v-if="!switches.length" size="sm" title="暂无切换记录" />
        </div>
      </template>
    </BaseCard>

    <PoolSettingsModal
      v-if="settings"
      v-model="settingsOpen"
      :settings="settings"
      :groups="groups"
      :accounts="accounts"
      :saving="saving"
      @save="save"
    />
  </div>
</template>

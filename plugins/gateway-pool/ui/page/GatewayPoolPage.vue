<script setup lang="ts">
import type { AccountQueue, DirectoryAccount, PoolView } from './model'
import { Activity, Hourglass, Network, RotateCcw, Settings2, Shuffle } from '@lucide/vue'
import { usePluginPolling } from '@sdk/polling'
import { call } from '@sdk/ui'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, ref, shallowRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { toast } from '@/components/base/BaseToast'
import AccountPoolCard from './components/AccountPoolCard.vue'
import PoolGroupEvents from './components/PoolGroupEvents.vue'
import PoolSettingsModal from './components/PoolSettingsModal.vue'
import PoolStatistics from './components/PoolStatistics.vue'

const view = ref<PoolView>()
const groups = ref<{ id: string, name: string }[]>([])
const accounts = ref<DirectoryAccount[]>([])
const error = shallowRef('')
const saving = shallowRef(false)
const savingQueue = shallowRef(false)
const settingsOpen = shallowRef(false)
const resetOpen = shallowRef(false)
const resetting = shallowRef(false)
const tab = shallowRef('accounts')
const now = shallowRef(Date.now() / 1000)
useIntervalFn(() => {
  now.value = Date.now() / 1000
}, 1000)

const names = computed(() => new Map(accounts.value.map(account => [account.id, account.name || account.email || account.id])))
const settings = computed(() => view.value?.settings)
const enabledGroups = computed(() => groups.value.filter(group => settings.value?.groupIds.includes(group.id)))
// 账号卡片：只显示选定分组的当前成员；已移出分组的账号不再显示，它们的历史记录仍在“轮转明细”里。
const accountCards = computed(() => {
  const pairs = view.value?.pairs ?? []
  const ids = new Set(accounts.value.filter(account => account.groups?.some(group => settings.value?.groupIds.includes(group.id))).map(account => account.id))
  return [...ids]
    .map(id => ({ id, name: names.value.get(id) ?? id, pairs: pairs.filter(pair => pair.accountId === id) }))
    .sort((a, b) => queuePosition(a.id) - queuePosition(b.id))
})
function queuePosition(id: string) {
  const index = view.value?.accountQueue.accounts.findIndex(item => item.accountId === id) ?? -1
  return index < 0 ? Infinity : index
}
function queueEntry(id: string) {
  return view.value?.accountQueue.accounts.find(item => item.accountId === id)
}
const accountColumns = defineTableColumns<(typeof accountCards.value)[number]>([
  { key: 'account', label: '账号与网关', kind: 'custom', size: '4xl' },
])
// 取消全部网关正在进行的静置：组合立即可复测，档位不变。
async function resetCooling() {
  if (resetting.value)
    return
  resetting.value = true
  try {
    const result = await call<{ changed: number }>('gateway-pool.cooling.reset', {})
    toast.success(result.changed ? `已取消 ${result.changed} 个网关的静置` : '没有正在静置的网关')
    resetOpen.value = false
  }
  catch (cause) {
    toast.error(cause instanceof Error ? cause.message : '取消静置失败')
  }
  finally {
    resetting.value = false
    void refresh()
  }
}
// 每次保存队列递增；保存前已发出的读取返回时版本已变，丢弃旧结果，避免开关被旧数据拨回去。
let queueVersion = 0
async function saveQueue(payload: { accountIds: string[] } | { accountId: string, enabled?: boolean, degradedUse?: boolean }) {
  if (savingQueue.value || !view.value)
    return
  savingQueue.value = true
  queueVersion++
  const previous = view.value.accountQueue
  // 开关先在页面上生效，保存失败再恢复。
  if ('accountId' in payload && queueEntry(payload.accountId))
    view.value.accountQueue = { ...previous, accounts: previous.accounts.map(item => item.accountId === payload.accountId ? { ...item, ...payload } : item) }
  try {
    // 首次操作开关时先保存完整可见顺序，避免新登记的账号跳到列表顶部。
    if ('accountId' in payload && accountCards.value.some(card => !queueEntry(card.id))) {
      view.value.accountQueue = await call<AccountQueue>('gateway-pool.queue.update', { accountIds: accountCards.value.map(card => card.id), revision: view.value.accountQueue.revision })
    }
    view.value.accountQueue = await call<AccountQueue>('gateway-pool.queue.update', { ...payload, revision: view.value.accountQueue.revision })
  }
  catch (cause) {
    if (view.value)
      view.value.accountQueue = previous
    toast.error(cause instanceof Error ? cause.message : '保存账号队列失败')
  }
  finally {
    savingQueue.value = false
    // 整页数据较大，后台刷新，不阻塞开关。
    void refresh()
  }
}
async function saveOrder(change: { originalIds: string[], orderedIds: string[] }) {
  if (JSON.stringify(change.originalIds) !== JSON.stringify(accountCards.value.map(card => card.id))) {
    toast.error('账号队列已变化，请重新拖动')
    return
  }
  await saveQueue({ accountIds: change.orderedIds })
}
const hourAgo = computed(() => now.value - 3600)
const recent = computed(() => (view.value?.observations ?? []).filter(sample => sample.at >= hourAgo.value))
const stats = computed(() => {
  // 统计只算当前分组成员，已移出分组的账号不计入。
  const members = new Set(accountCards.value.map(card => card.id))
  const pairs = (view.value?.pairs ?? []).filter(pair => members.has(pair.accountId))
  const switches = (view.value?.switches ?? []).filter(item => item.at >= hourAgo.value)
  return {
    serving: pairs.filter(pair => pair.state === 'ready').length,
    accounts: accountCards.value.length,
    cooling: pairs.filter(pair => pair.state === 'cooling' && (pair.restUntil ?? 0) > now.value).length,
    pairs: pairs.length,
    normal: recent.value.filter(sample => sample.classification === '21').length,
    degraded: recent.value.filter(sample => sample.classification === '29').length,
    failed: recent.value.filter(sample => sample.source !== 'probe-attempt' && sample.classification == null && sample.status != null && sample.status !== 200).length,
    switches: switches.length,
  }
})
const tabOptions = [
  { label: '账号与网关', value: 'accounts' },
  { label: '轮转明细', value: 'records' },
  { label: '轮转统计', value: 'statistics' },
  { label: '分组变动', value: 'groups' },
]
async function refresh() {
  if (savingQueue.value)
    return
  const version = queueVersion
  try {
    const response = await call<PoolView>('gateway-pool.read', { limit: 200 })
    if (savingQueue.value || version !== queueVersion)
      return
    view.value = response
    error.value = ''
    // 动态分组移入/移出完成后立刻重读目录，账号卡片随分组成员变化。
    const moved = (response.groupEvents ?? []).filter(event => event.status !== 'pending').map(event => `${event.id}:${event.status}`).join(',')
    if (movedSignature !== null && moved !== movedSignature)
      void loadDirectory()
    movedSignature = moved
  }
  catch (e) { error.value = e instanceof Error ? e.message : '读取网关池失败' }
}
let movedSignature: string | null = null
let loadingDirectory = false
async function loadDirectory() {
  if (loadingDirectory)
    return
  loadingDirectory = true
  try { await readDirectory() }
  finally { loadingDirectory = false }
}
async function readDirectory() {
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
// 管理员在账号页手动调整分组不产生分组变动记录，目录定期重读，移入和移出都会在一分钟内反映。
usePluginPolling(loadDirectory, shallowRef(60_000))
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
          <BaseButton :disabled="!view || !stats.cooling" @click="resetOpen = true">
            <template #icon>
              <RotateCcw class="size-4" />
            </template>
            取消静置
          </BaseButton>
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
          <Activity class="size-4 text-cp-info" />近期判定样本 · 1 小时内
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
          <Shuffle class="size-4 text-cp-primary-text" />近期切换样本 · 1 小时内
        </div>
        <div class="mt-2 text-2xl font-emphasis tabular-nums text-cp-text">
          {{ stats.switches }}<span class="ml-1 text-cp-sm text-cp-text-tertiary">次</span>
        </div>
      </div>
    </div>

    <BaseCard>
      <template #header>
        <div class="flex w-full flex-col gap-3 lg:flex-row lg:items-center">
          <BaseSegmented v-model="tab" label="网关池视图" :options="tabOptions" class="lg:w-96" />
        </div>
      </template>
      <template #body>
        <div v-if="tab === 'accounts'" class="grid gap-2.5">
          <p class="text-cp-xs text-cp-text-secondary">
            所有 Key 共用顺序，从上到下选择授权账号 · 一轮网关或额度用完后自动移到末尾
          </p>
          <BaseTable v-if="accountCards.length" :columns="accountColumns" :rows="accountCards" reorderable :reorder-disabled="savingQueue" @reorder="saveOrder">
            <template #account="{ row }">
              <AccountPoolCard
                :account-id="row.id"
                :name="row.name"
                :pairs="row.pairs"
                :window-seconds="settings?.windowSeconds ?? 180"
                :probe-model="settings?.probeModel ?? 'gpt-6.1-sol'"
                :mode="settings?.mode ?? 'lazy'"
                :catalog="view?.catalogs?.[row.id]"
                :now="now"
              >
                <template #actions>
                  <div class="flex flex-col items-end gap-1">
                    <BaseSwitch :model-value="queueEntry(row.id)?.enabled ?? true" :disabled="savingQueue" :label="`${row.name}参与网关池`" @update:model-value="saveQueue({ accountId: row.id, enabled: $event })" />
                    <BaseSwitch :model-value="queueEntry(row.id)?.degradedUse ?? false" :disabled="savingQueue" label="允许降智使用" show-label @update:model-value="saveQueue({ accountId: row.id, degradedUse: $event })" />
                    <span v-if="queueEntry(row.id)?.quotaExhausted" class="text-cp-xs text-cp-text-tertiary">额度已用完</span>
                  </div>
                </template>
              </AccountPoolCard>
            </template>
          </BaseTable>
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

        <PoolGroupEvents v-if="tab === 'groups'" :events="view?.groupEvents ?? []" :accounts="accounts" :groups="groups" :enabled="!!settings?.dynamicGroups" />

        <KeepAlive>
          <PoolStatistics v-if="tab === 'records' || tab === 'statistics'" :accounts="accounts" :mode="tab === 'statistics' ? 'summary' : 'records'" @records="tab = 'records'" />
        </KeepAlive>
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
    <BaseConfirmModal
      v-model="resetOpen"
      title="取消全部静置"
      :description="`${stats.cooling} 个网关正在静置。取消后立即可复测，静置档位不变，复测仍降智会照常重新静置。`"
      confirm-text="取消静置"
      :loading="resetting"
      @confirm="resetCooling"
    />
  </div>
</template>

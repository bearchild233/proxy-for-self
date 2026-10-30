<script setup lang="ts">
import type { WarmupAccount, WarmupModel, WarmupOverride, WarmupSettings } from '@/api/modules/accountWarmup'
import { FolderTree, ListChecks, RefreshCw } from '@lucide/vue'
import { computed, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { matchingWarmupAccounts, updateWarmupOverrides } from '../model'
import { useWarmupGroups } from '../useWarmupGroups'
import WarmupAccountRow from './WarmupAccountRow.vue'
import WarmupBatchModal from './WarmupBatchModal.vue'

const props = defineProps<{
  accounts: WarmupAccount[]
  names: Map<string, { name: string }>
  namesError: string
  disabled: boolean
  groupsSupported: boolean
  models: Record<string, WarmupModel[]>
  modelLoading: Set<string>
  modelErrors: Record<string, string>
}>()
const emit = defineEmits<{ names: [], models: [accountId: string, model: string] }>()
const settings = defineModel<WarmupSettings>({ required: true })
const { groups, loading: groupsLoading, error: groupsError, refresh: refreshGroups } = useWarmupGroups()
const search = ref('')
const groupSearch = ref('')
const groupFilter = ref('')
const page = ref(1)
const managedIds = ref<string[]>([])
const selectionError = ref('')
const batchOpen = ref(false)
const batchError = ref('')
const groupMap = computed(() => new Map(groups.value.map(group => [group.id, group])))
const selectedGroups = computed(() => new Set(settings.value.groupIds))
const targetAccounts = computed(() => matchingWarmupAccounts(settings.value, props.accounts))
const filtered = computed(() => {
  const query = search.value.trim().toLowerCase()
  return (settings.value.targetMode === 'groups' ? targetAccounts.value : props.accounts)
    .filter(account => (!groupFilter.value || account.groups?.includes(groupFilter.value)) && `${props.names.get(account.id)?.name ?? ''} ${account.id} ${account.provider}`.toLowerCase().includes(query))
})
const groupOptions = computed(() => [{ value: '', label: '全部分组' }, ...groups.value.map(group => ({ value: group.id, label: group.name }))])
const visibleGroups = computed(() => {
  const known = groups.value.map(group => ({ ...group, missing: false }))
  for (const id of settings.value.groupIds) {
    if (!groupMap.value.has(id))
      known.push({ id, name: `未知分组 · ${id}`, color: '', enabled: true, missing: true })
  }
  return known.filter(group => `${group.name} ${group.id}`.toLowerCase().includes(groupSearch.value.trim().toLowerCase()))
})
const pageCount = computed(() => Math.max(1, Math.ceil(filtered.value.length / 20)))
const currentPage = computed(() => Math.min(page.value, pageCount.value))
const visibleAccounts = computed(() => filtered.value.slice((currentPage.value - 1) * 20, currentPage.value * 20))
// 指定账号模式的勾选同时决定参与范围；全部/分组模式只用勾选来批量调整。
const selectedIds = computed(() => settings.value.targetMode === 'selected' ? settings.value.accountIds : managedIds.value)
const selectedSet = computed(() => new Set(selectedIds.value))
const managedAccounts = computed(() => (settings.value.targetMode === 'groups' ? targetAccounts.value : props.accounts).filter(account => selectedSet.value.has(account.id) && account.supported))
const selectablePage = computed(() => visibleAccounts.value.filter(account => account.supported))
const pageChecked = computed(() => selectablePage.value.length > 0 && selectablePage.value.every(account => selectedSet.value.has(account.id)))
const pageIndeterminate = computed(() => !pageChecked.value && selectablePage.value.some(account => selectedSet.value.has(account.id)))
function setSelected(ids: string[]) {
  const unique = [...new Set(ids)]
  if (unique.length > 500) {
    selectionError.value = '最多同时选择 500 个账号，请缩小筛选范围'
    return
  }
  if (settings.value.targetMode === 'selected')
    settings.value.accountIds = unique
  else managedIds.value = unique
  selectionError.value = ''
}
function selectAccount(id: string, selected: boolean) {
  setSelected(selected ? [...selectedIds.value, id] : selectedIds.value.filter(item => item !== id))
}
function selectMany(accounts: WarmupAccount[], selected = true) {
  const ids = accounts.filter(account => account.supported).map(account => account.id)
  setSelected(selected ? [...selectedIds.value, ...ids] : selectedIds.value.filter(id => !ids.includes(id)))
}
function toggleGroup(id: string, selected: boolean) {
  const ids = selected ? [...new Set([...settings.value.groupIds, id])] : settings.value.groupIds.filter(item => item !== id)
  if (ids.length > 500) {
    selectionError.value = '最多选择 500 个分组'
    return
  }
  settings.value.groupIds = ids
  page.value = 1
  managedIds.value = []
  selectionError.value = ''
}
function updateOverride(value: WarmupOverride) {
  if (!settings.value.accountOverrides.some(item => item.accountId === value.accountId) && settings.value.accountOverrides.length >= 500) {
    selectionError.value = '账号独立设置最多保留 500 个'
    return
  }
  settings.value.accountOverrides = [...settings.value.accountOverrides.filter(item => item.accountId !== value.accountId), value]
}
function loadModels(accountId: string, model: string) {
  emit('models', accountId, model)
}
function applyBatch(patch: Partial<Omit<WarmupOverride, 'accountId'>>) {
  try {
    settings.value = updateWarmupOverrides(settings.value, managedAccounts.value.map(account => account.id), patch)
    batchError.value = ''
    batchOpen.value = false
  }
  catch (cause) { batchError.value = cause instanceof Error ? cause.message : '批量设置未应用' }
}
</script>

<template>
  <BaseCard title="参与账号与分组" description="选择自动计划的范围，并批量管理账号的独立设置。">
    <template #actions>
      <span class="text-cp-sm text-cp-text-secondary">范围 {{ targetAccounts.length }} 个 · 勾选 {{ selectedIds.length }} 个</span>
    </template>
    <div class="flex flex-wrap items-center gap-3">
      <BaseSegmented v-model="settings.targetMode" label="自动预热账号范围" :options="[{ label: '全部当前账号', value: 'all' }, { label: '指定账号', value: 'selected' }, { label: '指定分组', value: 'groups', disabled: !groupsSupported }]" :disabled="disabled" @update:model-value="page = 1; managedIds = []" />
      <BaseButton size="sm" :loading="groupsLoading" :disabled="disabled" @click="refreshGroups(); emit('names')">
        <RefreshCw class="size-3.5" /> 刷新账号与分组
      </BaseButton>
    </div>
    <p v-if="!groupsSupported" role="status" class="mt-3 mb-0 text-cp-xs text-cp-warning-text">
      当前服务尚未确认动态分组能力，分组模式暂不可用；账号手动选择仍可使用。
    </p>
    <div v-if="settings.targetMode === 'groups'" class="mt-4 rounded-cp border border-solid border-cp-border-secondary p-4">
      <div class="mb-3 flex flex-wrap items-center gap-3">
        <FolderTree class="size-4 text-cp-primary" />
        <span class="text-cp-sm font-bold">选择一个或多个分组</span>
        <BaseInput v-model="groupSearch" class="min-w-48 flex-1" aria-label="搜索自动预热分组" placeholder="搜索分组" :disabled="disabled" />
        <BaseButton size="sm" :disabled="disabled || !settings.groupIds.length" @click="settings.groupIds = []; managedIds = []">
          清空分组
        </BaseButton>
      </div>
      <div class="grid max-h-64 gap-2 overflow-y-auto sm:grid-cols-2 lg:grid-cols-3">
        <div v-for="group in visibleGroups" :key="group.id" class="flex min-w-0 items-center gap-3 rounded-cp bg-cp-fill-quaternary p-3">
          <BaseCheckbox :model-value="selectedGroups.has(group.id)" :label="`选择分组 ${group.name}`" show-label :disabled="disabled || (!groupsSupported && !selectedGroups.has(group.id))" @update:model-value="toggleGroup(group.id, $event)">
            <template #label>
              {{ group.name }}
            </template>
          </BaseCheckbox>
          <span class="ml-auto shrink-0 text-cp-xs text-cp-text-secondary">{{ group.missing ? '目录未找到' : `${accounts.filter(account => account.groups?.includes(group.id)).length} 个账号` }}</span>
        </div>
      </div>
      <p v-if="!visibleGroups.length" class="my-3 text-cp-sm text-cp-text-secondary">
        {{ groupsLoading ? '正在读取分组…' : groupsError ? '分组目录读取失败，请刷新重试' : '暂无匹配分组' }}
      </p>
      <p class="mt-3 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        多个分组合并去重，成员加入或退出后自动跟随；分组的路由开关不改变账号预热资格，停用和养号保护仍按账号执行。批量独立设置只应用于当前选中的成员，新成员跟随全局默认。
      </p>
    </div>
    <div class="mt-4 flex flex-wrap items-center gap-3">
      <BaseInput v-model="search" class="min-w-48 flex-1" aria-label="搜索预热账号" placeholder="搜索账号名称或 ID" @update:model-value="page = 1" />
      <BaseSelect v-model="groupFilter" class="w-full sm:w-52" searchable :options="groupOptions" aria-label="筛选预热账号分组" @update:model-value="page = 1" />
      <BaseButton size="sm" :disabled="disabled || !filtered.length" @click="selectMany(filtered)">
        选中筛选结果
      </BaseButton>
      <BaseButton v-if="settings.targetMode === 'groups'" size="sm" :disabled="disabled || !targetAccounts.length || !groupsSupported" @click="selectMany(targetAccounts)">
        选中分组成员
      </BaseButton>
    </div>
    <div class="my-3 flex flex-wrap items-center gap-3 rounded-cp bg-cp-fill-quaternary p-3">
      <BaseCheckbox :model-value="pageChecked" :indeterminate="pageIndeterminate" label="选择本页账号" show-label :disabled="disabled || !selectablePage.length" @update:model-value="selectMany(visibleAccounts, $event)" />
      <span class="text-cp-xs text-cp-text-secondary">已勾选 {{ selectedIds.length }} 个，可管理 {{ managedAccounts.length }} 个，跨页保留</span>
      <BaseButton size="sm" :disabled="disabled || !selectedIds.length" @click="setSelected([])">
        清空勾选
      </BaseButton>
      <BaseButton size="sm" variant="primary" :disabled="disabled || !managedAccounts.length" @click="batchError = ''; batchOpen = true">
        <ListChecks class="size-3.5" /> 批量管理 ({{ managedAccounts.length }})
      </BaseButton>
    </div>
    <p class="my-3 text-cp-xs text-cp-text-secondary">
      {{ settings.targetMode === 'selected' ? '勾选决定指定账号范围；展开或批量管理可设置独立时间、模型、强度和延迟。' : '勾选仅用于批量调整独立设置，不改变全部账号或指定分组的参与范围。' }}
    </p>
    <p v-if="namesError || groupsError || selectionError" role="status" class="my-3 text-cp-xs text-cp-warning-text">
      {{ selectionError || groupsError || namesError }}
    </p>
    <div class="grid gap-2">
      <WarmupAccountRow v-for="account in visibleAccounts" :key="account.id" :account="account" :name="names.get(account.id)?.name || account.id" :settings="settings" :selected="selectedSet.has(account.id)" show-selection :disabled="disabled" :models="models[account.id] ?? []" :models-loading="modelLoading.has(account.id)" :models-error="modelErrors[account.id] ?? ''" @select="selectAccount(account.id, $event)" @override="updateOverride" @models="loadModels" />
    </div>
    <p v-if="!visibleAccounts.length" class="my-4 text-center text-cp-sm text-cp-text-secondary">
      {{ settings.targetMode === 'groups' && !settings.groupIds.length ? '先选择自动预热分组' : '暂无匹配账号' }}
    </p>
    <div v-if="pageCount > 1" class="mt-3 flex items-center justify-end gap-3">
      <BaseButton size="sm" :disabled="currentPage <= 1" @click="page = currentPage - 1">
        上一页
      </BaseButton>
      <span class="text-cp-xs text-cp-text-secondary">{{ currentPage }} / {{ pageCount }}</span>
      <BaseButton size="sm" :disabled="currentPage >= pageCount" @click="page = currentPage + 1">
        下一页
      </BaseButton>
    </div>
    <WarmupBatchModal v-model="batchOpen" :count="managedAccounts.length" :disabled="disabled" :error="batchError" @apply="applyBatch" />
  </BaseCard>
</template>

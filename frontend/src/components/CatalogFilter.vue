<script setup lang="ts">
import type { SelectOption } from '@/components/base/BaseSelect.vue'
import { computed, reactive, ref } from 'vue'
import { getAccountGroups, getAccounts, getApiKeys, getPricing } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

type CatalogKind = 'accounts' | 'keys' | 'groups' | 'models'
const props = defineProps<{
  kind: 'accounts' | 'keys' | 'groups' | 'usage' | 'errors' | 'quota'
  label: string
  supplementalOptions?: Partial<Record<CatalogKind, SelectOption[]>>
}>()
const model = defineModel<string>({ required: true })
const categorized = computed(() => ['usage', 'errors', 'quota'].includes(props.kind))
const category = ref<CatalogKind | 'custom'>(props.kind === 'quota' ? 'keys' : 'accounts')
const custom = ref(false)
const activeKind = computed(() => categorized.value ? category.value : props.kind as CatalogKind)
const manual = computed(() => categorized.value ? category.value === 'custom' : custom.value)
const catalogs = reactive<Record<CatalogKind, { choices: SelectOption[], loading: boolean, error: boolean, loadedAt: number }>>({
  accounts: { choices: [], loading: false, error: false, loadedAt: 0 },
  keys: { choices: [], loading: false, error: false, loadedAt: 0 },
  groups: { choices: [], loading: false, error: false, loadedAt: 0 },
  models: { choices: [], loading: false, error: false, loadedAt: 0 },
})
const categoryOptions = computed<SelectOption[]>(() => [
  { label: '账号', value: 'accounts' },
  { label: 'API 密钥', value: 'keys' },
  ...(props.kind === 'usage' ? [{ label: '模型', value: 'models' }] : []),
  { label: '关键词', value: 'custom' },
])
const catalog = computed(() => activeKind.value === 'custom' ? undefined : catalogs[activeKind.value])
const labels: Record<CatalogKind, string> = { accounts: '全部账号', keys: '全部 API 密钥', groups: '全部分组', models: '全部模型' }
const selectionLabel = computed(() => activeKind.value === 'custom' ? props.label : labels[activeKind.value])
// 历史用量可能只有账号快照；目录中的当前名称优先，但目录缺失不能使历史对象无法筛选。
const choices = computed(() => [...new Map([
  ...(activeKind.value === 'custom' ? [] : props.supplementalOptions?.[activeKind.value] ?? []),
  ...(catalog.value?.choices ?? []),
].map(option => [option.value, option])).values()])
const options = computed<SelectOption[]>(() => [
  { label: catalog.value?.loading ? '正在加载选项…' : selectionLabel.value, value: '' },
  ...(model.value && !choices.value.some(option => option.value === model.value)
    ? [{ label: model.value, value: model.value }]
    : []),
  ...choices.value,
])

function changeCategory(value: string) {
  if (!categoryOptions.value.some(option => option.value === value))
    return
  category.value = value as CatalogKind | 'custom'
  model.value = ''
  void load()
}

async function load() {
  if (manual.value || activeKind.value === 'custom')
    return
  // 每个类别独立缓存，切换类别不会混入尚未完成的上一类请求。
  const kind = activeKind.value
  const state = catalogs[kind]
  if (state.loading || Date.now() - state.loadedAt < 60_000)
    return
  state.loading = true
  state.error = false
  const items: SelectOption[] = []
  try {
    if (kind === 'accounts') {
      let page = 1
      let totalPages = 1
      do {
        const result = await getAccounts({ page, pageSize: 200 }, { silent: true })
        totalPages = result.page.totalPages
        items.push(...result.items.map(account => ({ label: account.name, description: account.email || undefined, value: account.id })))
        page += 1
      } while (page <= totalPages)
    }
    if (kind === 'keys') {
      let cursor: string | undefined
      do {
        const result = await getApiKeys({ cursor, limit: 200 }, { silent: true })
        items.push(...result.items.map(key => ({ label: key.name, description: key.label || undefined, value: props.kind === 'keys' ? key.name : key.id })))
        cursor = result.nextCursor || undefined
      } while (cursor)
    }
    if (kind === 'groups') {
      let page = 1
      let totalPages = 1
      do {
        const result = await getAccountGroups({ page, pageSize: 200 }, { silent: true })
        totalPages = result.page.totalPages
        items.push(...result.items.map(group => ({ label: group.name, value: group.name })))
        page += 1
      } while (page <= totalPages)
    }
    if (kind === 'models') {
      const pricing = await getPricing({ silent: true })
      const models = new Set(Object.values(pricing.defaults).flatMap(provider => Object.keys(provider)))
      for (const catalog of [pricing.synced, pricing.overrides]) {
        for (const provider of Object.values(catalog)) {
          for (const model of Object.keys(provider))
            models.add(model)
        }
      }
      items.push(...[...models].sort().map(model => ({ label: model, value: model })))
    }
    state.choices = [...new Map(items.map(item => [item.value, item])).values()]
    state.loadedAt = Date.now()
  }
  catch {
    if (items.length)
      state.choices = items
    state.error = true
  }
  finally {
    state.loading = false
  }
}
</script>

<template>
  <div class="min-w-0" @focusin="load" @pointerdown="load">
    <div class="flex min-w-0 flex-wrap items-center gap-2">
      <BaseSelect v-if="categorized" :model-value="category" :options="categoryOptions" aria-label="筛选类别" class="w-28 shrink-0" @update:model-value="changeCategory" />
      <BaseSelect v-if="!manual" v-model="model" :options="options" :aria-label="selectionLabel" searchable class="min-w-40 flex-1" />
      <BaseInput v-else v-model="model" aria-label="输入 ID 或关键词筛选" placeholder="请求 ID / 关键词" class="min-w-40 flex-1" />
      <BaseButton v-if="!categorized" variant="ghost" class="shrink-0" @click="custom = !custom; load()">
        {{ custom ? '选项' : '手动输入' }}
      </BaseButton>
      <BaseButton v-if="model" variant="ghost" class="shrink-0" aria-label="清除筛选" @click="model = ''">
        清除
      </BaseButton>
    </div>
    <p v-if="!manual && catalog?.error" role="status" class="mt-1 text-cp-xs text-cp-text-secondary">
      选项加载失败，重新打开可重试
    </p>
  </div>
</template>

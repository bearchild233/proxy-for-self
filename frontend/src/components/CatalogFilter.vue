<script setup lang="ts">
import type { SelectOption } from '@/components/base/BaseSelect.vue'
import { computed, ref } from 'vue'
import { getAccountGroups, getAccounts, getApiKeys, getPricing } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

const props = defineProps<{
  kind: 'accounts' | 'keys' | 'groups' | 'usage' | 'errors'
  label: string
}>()
const model = defineModel<string>({ required: true })
const choices = ref<SelectOption[]>([])
const loading = ref(false)
const error = ref(false)
const custom = ref(false)
let loadedAt = 0
const options = computed(() => [
  { label: loading.value ? '正在加载选项…' : props.label, value: '' },
  ...(model.value && !choices.value.some(option => option.value === model.value)
    ? [{ label: model.value, value: model.value }]
    : []),
  ...choices.value,
])

async function load() {
  if (loading.value || Date.now() - loadedAt < 60_000)
    return
  loading.value = true
  error.value = false
  const items: SelectOption[] = []
  try {
    if (['accounts', 'usage', 'errors'].includes(props.kind)) {
      let page = 1
      let totalPages = 1
      do {
        const result = await getAccounts({ page, pageSize: 200 }, { silent: true })
        totalPages = result.page.totalPages
        items.push(...result.items.map(account => ({
          label: `${props.kind === 'accounts' ? '' : '账号 · '}${account.name}`,
          description: account.email || undefined,
          value: account.id,
        })))
        page += 1
      } while (page <= totalPages)
    }
    if (['keys', 'usage', 'errors'].includes(props.kind)) {
      let cursor: string | undefined
      do {
        const result = await getApiKeys({ cursor, limit: 200 }, { silent: true })
        items.push(...result.items.map(key => ({
          label: `${props.kind === 'keys' ? '' : 'Key · '}${key.name}`,
          description: key.label || undefined,
          value: props.kind === 'keys' ? key.name : key.id,
        })))
        cursor = result.nextCursor || undefined
      } while (cursor)
    }
    if (props.kind === 'groups') {
      let page = 1
      let totalPages = 1
      do {
        const result = await getAccountGroups({ page, pageSize: 200 }, { silent: true })
        totalPages = result.page.totalPages
        items.push(...result.items.map(group => ({ label: group.name, value: group.name })))
        page += 1
      } while (page <= totalPages)
    }
    if (props.kind === 'usage') {
      const pricing = await getPricing({ silent: true })
      const models = new Set(Object.values(pricing.defaults).flatMap(provider => Object.keys(provider)))
      for (const catalog of [pricing.synced, pricing.overrides]) {
        for (const provider of Object.values(catalog)) {
          for (const model of Object.keys(provider))
            models.add(model)
        }
      }
      items.push(...[...models].sort().map(model => ({ label: `模型 · ${model}`, value: model })))
    }
    choices.value = [...new Map(items.map(item => [item.value, item])).values()]
    loadedAt = Date.now()
  }
  catch {
    // 部分目录失败时保留已加载的选项，下次打开可重试。
    choices.value = items
    error.value = true
  }
  finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="min-w-0" @focusin="load" @pointerdown="load">
    <div class="flex min-w-0 items-center gap-2">
      <BaseSelect v-if="!custom" v-model="model" :options="options" :aria-label="label" class="min-w-0 flex-1" />
      <BaseInput v-else v-model="model" aria-label="输入 ID 或关键词筛选" placeholder="输入 ID 或关键词" class="min-w-0 flex-1" />
      <BaseButton variant="ghost" class="shrink-0" @click="custom = !custom">
        {{ custom ? '选项' : '手动输入' }}
      </BaseButton>
      <BaseButton v-if="model" variant="ghost" class="shrink-0" aria-label="清除筛选" @click="model = ''">
        清除
      </BaseButton>
    </div>
    <p v-if="error" role="status" class="mt-1 text-cp-xs text-cp-text-secondary">
      部分选项加载失败，重新打开可重试
    </p>
  </div>
</template>

<script setup lang="ts">
import { Plus, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import CatalogFilter from '@/components/CatalogFilter.vue'
import { accountGroupStatusOptions } from '../constants'

defineProps<{
  batchDeleting: boolean
  selectedCount: number
}>()
const emit = defineEmits<{
  create: []
  deleteSelected: []
}>()
const search = defineModel<string>('search', { required: true })
const status = defineModel<string>('status', { required: true })
</script>

<template>
  <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
    <CatalogFilter v-model="search" kind="groups" label="全部分组名称" class="min-w-0 w-full xl:max-w-xl" />
    <BaseSelect
      v-model="status"
      :options="accountGroupStatusOptions"
      aria-label="按分组状态筛选"
      class="w-40"
    />
    <div class="flex shrink-0 items-center justify-end gap-2 sm:ml-auto">
      <BaseButton
        v-if="selectedCount > 0"
        variant="destructive"
        :disabled="batchDeleting"
        @click="emit('deleteSelected')"
      >
        <template #icon>
          <Trash2 class="size-4" />
        </template>
        删除选中 ({{ selectedCount }})
      </BaseButton>
      <BaseButton variant="primary" @click="emit('create')">
        <template #icon>
          <Plus class="size-4" />
        </template>
        创建分组
      </BaseButton>
    </div>
  </div>
</template>

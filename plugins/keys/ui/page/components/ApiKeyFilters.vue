<script setup lang="ts">
import { Plus, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import CatalogFilter from '@/components/CatalogFilter.vue'

defineProps<{
  batchDeleting: boolean
  selectedCount: number
}>()

const emit = defineEmits<{
  create: []
  deleteSelected: []
}>()

const search = defineModel<string>('search', { required: true })
</script>

<template>
  <div
    class="flex w-full flex-wrap items-center gap-3"
    role="group"
    aria-label="API Key 筛选与操作"
  >
    <div class="min-w-0 flex-1 md:w-96 md:flex-none">
      <CatalogFilter v-model="search" kind="keys" label="全部 API Key" class="min-w-0 w-full xl:max-w-xl" />
    </div>

    <div class="flex shrink-0 items-center justify-end gap-2 md:ml-auto">
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
        创建 API Key
      </BaseButton>
    </div>
  </div>
</template>

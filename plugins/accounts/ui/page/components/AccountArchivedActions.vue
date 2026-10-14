<script setup lang="ts">
import type { AccountRow } from '../constants'
import { ArchiveRestore, KeyRound, Trash2 } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'

// 已归档账号只提供三项操作：重新授权（成功后自动启用并离开归档）、恢复到主列表（保持停用）、删除。
defineProps<{
  account: AccountRow
  restoring: boolean
  deleting: boolean
}>()

const emit = defineEmits<{
  reauthorize: [account: AccountRow]
  restore: [account: AccountRow]
  delete: [account: AccountRow]
}>()
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
    <BaseIconButton
      v-if="account.authenticationKind === 'oauth'"
      variant="ghost"
      size="sm"
      label="重新授权"
      @click.stop="emit('reauthorize', account)"
    >
      <KeyRound class="size-3.5 text-cp-link" />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      label="恢复到主列表"
      :loading="restoring"
      @click.stop="emit('restore', account)"
    >
      <ArchiveRestore class="size-3.5 text-cp-link" />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      label="删除账号"
      :disabled="deleting"
      @click.stop="emit('delete', account)"
    >
      <Trash2 class="size-3.5 text-cp-error" />
    </BaseIconButton>
  </div>
</template>

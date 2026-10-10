<script setup lang="ts">
import type { AccountRow } from '../constants'
import type { OAuthExportFormat } from '../utils/oauthExport'
import { Download, KeyRound, MoreHorizontal, Pencil, RefreshCw, RotateCcw, Trash2, Wifi } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BasePopover from '@/components/base/BasePopover.vue'

defineProps<{
  account: AccountRow
  deleting: boolean
  recovering: boolean
  refreshing: boolean
  testing: boolean
  diagnosticsEnabled: boolean
  exportingOAuth: boolean
}>()

const emit = defineEmits<{
  edit: [account: AccountRow]
  delete: [account: AccountRow]
  recover: [accountId: string]
  test: [account: AccountRow]
  refresh: [accountId: string]
  reauthorize: [account: AccountRow]
  exportOAuth: [account: AccountRow, format: OAuthExportFormat]
}>()
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="编辑账号"
      @click.stop="emit('edit', account)"
    >
      <Pencil class="size-3.5 text-cp-link" />
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

    <BasePopover placement="bottom-end">
      <template #trigger="{ open }">
        <BaseIconButton variant="ghost" size="sm" label="更多操作" :pressed="open">
          <MoreHorizontal class="size-4" />
        </BaseIconButton>
      </template>

      <template #default="{ close }">
        <div class="w-48 p-1.5">
          <BaseMenuItem
            v-if="diagnosticsEnabled"
            :loading="testing"
            :disabled="testing"
            @click.stop="(close(), emit('test', account))"
          >
            <template #icon>
              <Wifi class="size-3.5 text-cp-text-quaternary" />
            </template>
            账号诊断
          </BaseMenuItem>
          <BaseMenuItem
            v-if="account.authenticationKind === 'oauth'"
            :loading="refreshing"
            :disabled="refreshing"
            @click.stop="(close(), emit('refresh', account.id))"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin text-cp-text-quaternary motion-reduce:animate-none" />
            </template>
            <template #icon>
              <RefreshCw class="size-3.5 text-cp-text-quaternary" />
            </template>
            刷新令牌
          </BaseMenuItem>
          <BaseMenuItem v-if="account.authenticationKind === 'oauth'" @click.stop="(close(), emit('reauthorize', account))">
            <template #icon>
              <KeyRound class="size-3.5 text-cp-text-quaternary" />
            </template>
            重新授权
          </BaseMenuItem>
          <BaseMenuItem
            v-if="account.authenticationKind === 'oauth'"
            :loading="exportingOAuth"
            :disabled="exportingOAuth"
            @click.stop="(close(), emit('exportOAuth', account, 'proxy'))"
          >
            <template #icon>
              <Download class="size-3.5 text-cp-text-quaternary" />
            </template>
            导出 OAuth JSON
          </BaseMenuItem>
          <BaseMenuItem
            v-if="account.authenticationKind === 'oauth' && account.provider === 'openai'"
            :disabled="exportingOAuth"
            @click.stop="(close(), emit('exportOAuth', account, 'codex'))"
          >
            <template #icon>
              <Download class="size-3.5 text-cp-text-quaternary" />
            </template>
            导出 Codex auth.json
          </BaseMenuItem>
          <BaseMenuItem
            :loading="recovering"
            :disabled="recovering"
            @click.stop="(close(), emit('recover', account.id))"
          >
            <template #icon>
              <RotateCcw class="size-3.5 text-cp-text-quaternary" />
            </template>
            恢复状态
          </BaseMenuItem>
        </div>
      </template>
    </BasePopover>
  </div>
</template>

<script setup lang="ts">
import type { SubscriptionState } from '../../composables/useAccountSubscriptions'
import type { AccountRow } from '../../constants'
import { RefreshCw, UserRound } from '@lucide/vue'

import { computed, shallowRef } from 'vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { formatDateTime } from '@/utils/date'
import { groupedAccountQuotaWindows, orderedPanelQuotaWindows } from '../../constants'
import AccountProfileModal from '../AccountProfileModal/index.vue'
import AccountQuotaPanelEntry from './Entry.vue'
import AccountResetCredits from './ResetCredits.vue'

const props = defineProps<{
  account: AccountRow
  refreshing: boolean
  subscriptionState?: SubscriptionState
  saving: boolean
}>()

const emit = defineEmits<{
  refreshQuota: [accountId: string]
  quotaReset: [accountId: string]
  expiryPriorityChange: [value: boolean]
}>()

const quotaEntries = computed(() => groupedAccountQuotaWindows(
  orderedPanelQuotaWindows(props.account.quota.windows),
))
const profileOpen = shallowRef(false)
const expiresAt = computed(() => props.subscriptionState?.subscription?.expiresAt ?? props.account.lifecycle.subscriptionExpiresAt)
const canPrioritize = computed(() => !props.account.lifecycle.archived && props.account.planType?.toLowerCase() === 'plus')
</script>

<template>
  <section class="flex min-h-0 flex-col rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary">
    <div class="mb-3 flex shrink-0 items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="m-0 text-cp-lg font-heavy text-cp-text">
          账号额度
        </h3>
        <p
          v-if="account.authenticationKind !== 'api_key'"
          class="m-0 mt-1 flex min-w-0 items-center gap-1.5 text-cp-xs font-emphasis text-cp-text-secondary"
        >
          <span>{{ account.provider === 'xai' ? 'xAI 用量窗口' : 'Codex 额度' }}</span>
          <span>·</span>
          <span>最近刷新: {{ account.quota.refreshedAtDisplay }}</span>
        </p>
      </div>
      <div v-if="account.authenticationKind !== 'api_key'" class="flex shrink-0 items-center gap-0.5">
        <BaseIconButton
          v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'"
          label="查看个人信息"
          size="sm"
          variant="ghost"
          :pressed="profileOpen"
          @click="profileOpen = true"
        >
          <UserRound class="size-3.5" />
        </BaseIconButton>
        <AccountResetCredits
          v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'"
          :account="account"
          @consumed="emit('quotaReset', $event)"
        />
        <BaseIconButton
          variant="ghost"
          size="sm"
          label="刷新额度"
          :loading="refreshing"
          :disabled="refreshing"
          @click="emit('refreshQuota', account.id)"
        >
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </BaseIconButton>
      </div>
    </div>

    <div v-if="account.authenticationKind === 'api_key'" class="grid flex-1 place-items-center">
      <BaseEmpty title="暂不支持查询上游额度" surface="none" />
    </div>
    <div v-else class="grid min-h-0 gap-3">
      <AccountQuotaPanelEntry
        v-for="entry in quotaEntries"
        :key="entry.key"
        :label="entry.label"
        :windows="entry.windows"
      />
      <p v-if="quotaEntries.length === 0" class="m-0 text-cp-sm font-emphasis text-cp-text-secondary">
        额度待观测
      </p>
    </div>
    <div
      v-if="account.provider === 'openai' && account.authenticationKind === 'oauth' && (expiresAt || canPrioritize)"
      class="mt-5 grid gap-3 text-cp-xs text-cp-text-secondary"
    >
      <div v-if="expiresAt" class="flex flex-wrap items-baseline gap-x-3 gap-y-1" :title="subscriptionState?.failed ? '更新失败，显示上次数据' : undefined">
        <span>订阅到期</span>
        <time :datetime="expiresAt">{{ formatDateTime(expiresAt) }}</time>
        <span v-if="subscriptionState?.subscription?.willRenew != null">{{ subscriptionState.subscription.willRenew ? '自动续费' : '不自动续费' }}</span>
      </div>
      <BaseCheckbox
        v-if="canPrioritize"
        :model-value="account.lifecycle.expiryPriority" :disabled="saving" label="临期优先" show-label
        title="订阅到期前 5 天自动提高到 100；续费后恢复基础权重"
        @update:model-value="emit('expiryPriorityChange', $event)"
      />
    </div>
  </section>

  <AccountProfileModal
    v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'"
    v-model="profileOpen"
    :account="account"
  />
</template>

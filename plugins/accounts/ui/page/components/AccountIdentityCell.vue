<script setup lang="ts">
import type { getAccounts } from '@/api'
import { Grok, Openai } from '@boxicons/vue'
import { CircleHelp } from '@lucide/vue'
import { computed } from 'vue'
import AccountNotesPopover from './AccountNotesPopover.vue'
import AccountPlanBadge from '@kit/accounts/AccountPlanBadge.vue'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type AccountIdentity = Pick<AccountRow, 'id' | 'email' | 'planType' | 'planTypeDisplay' | 'provider'>
  & Partial<Pick<AccountRow, 'accountId' | 'notes' | 'name' | 'authenticationKind'>>

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
    showNotes?: boolean
    titleMode?: 'local-part' | 'email'
    metaPosition?: 'title' | 'secondary'
    metaSize?: 'xs' | 'sm'
  }>(),
  {
    size: 'md',
    showPlan: false,
    showNotes: false,
    titleMode: 'local-part',
    metaPosition: 'title',
    metaSize: 'sm',
  },
)

const emailText = computed(() => {
  const email = props.account.email?.trim()
  if (email)
    return email
  if ('accountId' in props.account && typeof props.account.accountId === 'string')
    return props.account.accountId
  return String(props.account.id)
})

const visibleNotes = computed(() => props.showNotes ? props.account.notes : undefined)

const displayTitle = computed(() =>
  props.account.name?.trim() || (visibleNotes.value || props.titleMode === 'email' || props.account.authenticationKind === 'api_key' ? emailText.value : emailText.value.split('@')[0]),
)

const secondaryText = computed(() =>
  props.account.name?.trim() ? props.account.email : props.titleMode === 'email' || props.account.authenticationKind === 'api_key' ? null : emailText.value,
)

const provider = computed(() => props.account.provider.trim().toLowerCase())
const avatarLabel = computed(() => provider.value === 'openai' ? 'Codex' : provider.value === 'xai' ? 'Grok' : '未知平台')

const avatarSizeClass = computed(() =>
  props.size === 'lg' ? 'size-10 text-cp-xl' : 'size-9 text-cp',
)

const secondaryClass = computed(() =>
  props.size === 'lg'
    ? 'mt-1 text-cp-sm text-cp-text-secondary'
    : 'mt-0.5 font-mono text-cp-xs text-cp-text-quaternary',
)

const metaGapClass = computed(() => props.metaSize === 'xs' ? 'gap-1' : 'gap-1.5')
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      class="inline-flex shrink-0 items-center justify-center rounded-lg bg-cp-fill-quaternary text-cp-text"
      :class="avatarSizeClass"
      role="img"
      :aria-label="avatarLabel"
      :title="avatarLabel"
    >
      <Openai v-if="provider === 'openai'" class="size-6" aria-hidden="true" />
      <Grok v-else-if="provider === 'xai'" class="size-6" aria-hidden="true" />
      <CircleHelp v-else class="size-6" aria-hidden="true" />
    </span>
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-2">
        <span class="min-w-0 flex-1 truncate text-cp font-heavy text-cp-text" :title="displayTitle">
          {{ displayTitle }}
        </span>
        <span
          v-if="metaPosition === 'title' && (showPlan || $slots.meta)"
          class="inline-flex shrink-0 items-center justify-end"
          :class="metaGapClass"
        >
          <slot name="meta" />
          <AccountPlanBadge v-if="showPlan" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
        </span>
      </div>
      <div
        v-if="metaPosition === 'secondary' && (showPlan || $slots.meta)"
        class="mt-0.5 inline-flex min-w-0 items-center"
        :class="metaGapClass"
      >
        <slot name="meta" />
        <AccountPlanBadge v-if="showPlan" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
      </div>
      <AccountNotesPopover v-else-if="visibleNotes" :notes="visibleNotes" class="mt-0.5" />
      <div v-else-if="secondaryText" class="truncate font-emphasis" :class="secondaryClass">
        {{ secondaryText }}
      </div>
    </div>
  </div>
</template>

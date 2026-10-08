import { usePluginVisibility } from '@sdk/visibility'
import type { Ref } from 'vue'
import type { Account, AccountSubscription } from '@/api'
import { useIntervalFn } from '@vueuse/core'
import { onScopeDispose, reactive, ref, watch } from 'vue'
import { getAccountPersonalInfo } from '@/api'

export interface SubscriptionState {
  subscription: AccountSubscription | null
  loading: boolean
  failed: boolean
  checkedAt: number
  identity: string
}

function identity(account: Account) {
  return JSON.stringify([account.accountId, account.userId, account.authenticationKind, account.planType])
}

export function useAccountSubscriptions(accounts: Ref<Account[]>) {
  const states = reactive<Record<string, SubscriptionState>>({})
  const refreshing = ref(false)
  const visibility = usePluginVisibility()
  const controller = new AbortController()
  let active: Promise<void> | undefined
  let pending = false

  async function load(force = false) {
    if (active) {
      pending = true
      return active
    }
    if (controller.signal.aborted || visibility.value !== 'visible')
      return
    const queue = accounts.value.filter(account => account.enabled && account.provider === 'openai' && account.authenticationKind === 'oauth')
    // 仅加载当前页，最多两个并发；五分钟缓存避免额度自动刷新反复请求订阅。
    async function worker() {
      while (queue.length && !controller.signal.aborted && visibility.value === 'visible') {
        const account = queue.shift()!
        if (!accounts.value.some(current => current.id === account.id && identity(current) === identity(account)))
          continue
        let state = states[account.id]
        if (!state || state.identity !== identity(account)) {
          states[account.id] = { subscription: null, loading: false, failed: false, checkedAt: 0, identity: identity(account) }
          state = states[account.id]!
        }
        if (!force && Date.now() - state.checkedAt < 300_000)
          continue
        state.loading = true
        try {
          const result = await getAccountPersonalInfo({ accountId: account.id }, { silent: true, signal: controller.signal })
          if (controller.signal.aborted)
            return
          // 未返回订阅可能是上游不可用，不能据此断言账号没有订阅。
          state.failed = !result.subscription
          state.subscription = result.subscription ?? state.subscription
        }
        catch {
          if (!controller.signal.aborted)
            state.failed = true
        }
        finally {
          state.loading = false
          state.checkedAt = Date.now()
        }
      }
    }
    refreshing.value = true
    active = Promise.all([worker(), worker()]).then(() => {})
    try {
      await active
    }
    finally {
      active = undefined
      refreshing.value = false
      if (pending) {
        pending = false
        void load()
      }
    }
  }

  watch(accounts, () => {
    for (const account of accounts.value) {
      if (states[account.id] && states[account.id]!.identity !== identity(account))
        delete states[account.id]
    }
    void load()
  }, { immediate: true })
  watch(visibility, (value) => {
    if (value === 'visible')
      void load()
  })
  useIntervalFn(() => {
    void load()
  }, 60_000)
  onScopeDispose(() => controller.abort())
  return { subscriptionStates: states, subscriptionsRefreshing: refreshing, refreshSubscriptions: () => load(true) }
}

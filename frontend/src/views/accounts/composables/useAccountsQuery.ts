import type { AccountAttention } from '../utils/accountAttention'
import type { AccountListResponse, AccountPersonalInfoResponse } from '@/api'
import type { RequestOptions } from '@/api/request'
import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { useDocumentVisibility, useIntervalFn, watchDebounced } from '@vueuse/core'

import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountPersonalInfo, getAccounts, refreshAccountQuota } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { accountNeedsAttention } from '../utils/accountAttention'
import { refreshAccountPage } from '../utils/refreshAccountPage'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountsQuery() {
  const refreshing = shallowRef(false)
  const lastRefreshedAt = shallowRef('')
  const refreshMessage = shallowRef('')
  let syncController: AbortController | undefined
  const visibility = useDocumentVisibility()
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const statusQuery = shallowRef('')
  const groupQuery = shallowRef('')
  const attentionQuery = shallowRef<AccountAttention>('')
  const attentionNote = shallowRef('')
  const subscriptions = new Map<string, { identity: string, checkedAt: number, value: AccountPersonalInfoResponse['subscription'] }>()
  const sort = shallowRef<BaseTableSort>()
  const accountSummary = shallowRef({
    total: 0,
    normal: 0,
    quotaExhausted: 0,
    rateLimited: 0,
    disabled: 0,
    error: 0,
  })

  const query = usePagedQuery({
    initialPageSize: 20,
    load: loadPage,
    onSuccess: (result) => {
      accountSummary.value = result.summary
    },
  })

  async function loadPage({ page, pageSize }: { page: number, pageSize: number }, options: RequestOptions): Promise<AccountListResponse> {
    const attention = attentionQuery.value
    const params = {
      page,
      pageSize,
      search: searchQuery.value,
      provider: providerQuery.value || undefined,
      status: statusQuery.value || undefined,
      groupId: groupQuery.value || undefined,
      sortBy: sort.value?.key,
      sortDirection: sort.value?.direction,
    }
    if (!attention) {
      attentionNote.value = ''
      return getAccounts(params, options)
    }
    // 先读取全部匹配目录再筛选、分页，避免只筛当前页造成漏报。
    const first = await getAccounts({ ...params, page: 1, pageSize: 200 }, options)
    const all = [...first.items]
    for (let next = 2; next <= first.page.totalPages; next++) {
      options.signal?.throwIfAborted()
      const result = await getAccounts({ ...params, page: next, pageSize: 200 }, options)
      all.push(...result.items)
    }
    if (attention === 'expiring' || attention === 'subscription_unknown') {
      const pending = all.filter(account => account.enabled && account.provider === 'openai' && account.authenticationKind === 'oauth')
      let completed = 0
      const total = pending.length
      async function worker() {
        while (pending.length) {
          options.signal?.throwIfAborted()
          const account = pending.shift()!
          const identity = JSON.stringify([account.accountId, account.userId, account.planType, account.authenticationKind])
          const cached = subscriptions.get(account.id)
          if (!cached || cached.identity !== identity || Date.now() - cached.checkedAt > 300_000) {
            let value: AccountPersonalInfoResponse['subscription'] = null
            try {
              value = (await getAccountPersonalInfo({ accountId: account.id }, { ...options, silent: true })).subscription
            }
            catch {
              options.signal?.throwIfAborted()
            }
            subscriptions.set(account.id, { identity, checkedAt: Date.now(), value })
          }
          completed++
          attentionNote.value = `订阅信息已检查 ${completed}/${total}`
        }
      }
      await Promise.all([worker(), worker()])
      const unknown = all.filter(account => accountNeedsAttention(account, 'subscription_unknown', subscriptions.get(account.id)?.value)).length
      attentionNote.value = `已检查全部匹配账号；${unknown} 个订阅信息未知，可切换“订阅未知”查看。订阅结果缓存 5 分钟，手动刷新可重新读取。`
    }
    else {
      attentionNote.value = attention === 'low_quota' ? '任一已知额度窗口剩余 ≤10%，或已确认额度耗尽；未知额度不视为充足。' : '凭据无效、过期或身份未确认；不自动重新授权。'
    }
    options.signal?.throwIfAborted()
    const items = all.filter(account => accountNeedsAttention(account, attention, subscriptions.get(account.id)?.value))
    return { ...first, items: items.slice((page - 1) * pageSize, page * pageSize), page: { page, pageSize, total: items.length, totalPages: Math.max(1, Math.ceil(items.length / pageSize)) } }
  }

  async function refreshAccounts(silent = false) {
    if (query.loading.value || refreshing.value)
      return
    refreshing.value = true
    if (!silent)
      subscriptions.clear()
    const controller = new AbortController()
    syncController = controller
    refreshMessage.value = ''
    try {
      const result = await refreshAccountPage(
        [...query.items.value],
        accountId => refreshAccountQuota({ accountId }, { silent: true, signal: controller.signal }),
        controller.signal,
      )
      if (controller.signal.aborted)
        return
      // 即使额度接口报错也要回读：后端可能已保存令牌失效等最新状态。
      const loaded = await query.execute({ background: true, silent })
      if (controller.signal.aborted)
        return
      if (result.succeeded > 0 && loaded)
        lastRefreshedAt.value = new Date().toLocaleTimeString()
      if (!loaded)
        refreshMessage.value = '账号列表回读失败，请重试'
      else if (result.failed.length)
        refreshMessage.value = `已同步 ${result.succeeded} 个，${result.failed.length} 个未同步：${result.failed.join('、')}；请在账号额度面板重试查看原因`
      else if (result.succeeded === 0)
        refreshMessage.value = '当前页没有可同步上游额度的启用账号，已重读本地状态'
      else if (result.skipped)
        refreshMessage.value = `已同步 ${result.succeeded} 个；${result.skipped} 个停用或不支持额度查询的账号仅重读本地状态`
      if (!silent) {
        if (result.failed.length || !loaded)
          toast.warning(refreshMessage.value)
        else
          toast.success(refreshMessage.value || `已同步当前页 ${result.succeeded} 个账号的状态与额度`)
      }
    }
    finally {
      refreshing.value = false
    }
  }

  useIntervalFn(() => {
    if (visibility.value === 'visible')
      void refreshAccounts(true)
  }, 30_000)
  watch(visibility, (value) => {
    if (value === 'visible')
      void refreshAccounts(true)
    else
      syncController?.abort()
  })

  const accountPagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  watch([query.page, query.pageSize, searchQuery, providerQuery, statusQuery, groupQuery, attentionQuery, sort], () => {
    // 翻页或切换筛选后，不把上一页的同步结果显示成当前页已更新。
    syncController?.abort()
    lastRefreshedAt.value = ''
    refreshMessage.value = ''
  })

  function handlePageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    query.page.value = 1
    void query.execute()
  }

  function handleSortChange(nextSort: BaseTableSort | undefined) {
    sort.value = nextSort
    query.page.value = 1
    void query.execute()
  }

  async function replaceAccount(updated: AccountRow) {
    // 先取消旧查询并应用接口返回的账号，避免旧响应覆盖最新行数据。
    query.invalidate()
    query.items.value = query.items.value.map(account => account.id === updated.id ? updated : account)

    // 筛选、排序、概览和末页回退仍由回读校准，但不触发整表加载。
    if (!await query.execute({ background: true }))
      return true // 回读失败或被新查询取代时，不依据旧页面取消选择。
    return query.items.value.some(account => account.id === updated.id)
  }

  watchDebounced(
    searchQuery,
    () => {
      query.page.value = 1
      void query.execute()
    },
    { debounce: 250 },
  )

  watch([providerQuery, statusQuery, groupQuery, attentionQuery], () => {
    query.page.value = 1
    void query.execute()
  })

  onMounted(async () => {
    if (await query.execute() && visibility.value === 'visible')
      void refreshAccounts(true)
  })
  onScopeDispose(() => syncController?.abort())

  return {
    page: query.page,
    pageSize: query.pageSize,
    totalAccounts: query.total,
    loading: query.loading,
    refreshing,
    lastRefreshedAt,
    refreshMessage,
    refreshAccounts,
    accounts: query.items,
    loadAccounts: query.execute,
    refreshAccountsSilently: () => query.execute({ silent: true }),
    searchQuery,
    providerQuery,
    statusQuery,
    groupQuery,
    attentionQuery,
    attentionNote,
    sort,
    accountSummary,
    accountPagination,
    replaceAccount,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}

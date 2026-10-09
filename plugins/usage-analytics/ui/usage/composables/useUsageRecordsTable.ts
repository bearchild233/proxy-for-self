import type { Ref } from 'vue'
import type { UsageDisplayRecord } from '../utils/records'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import type { UsageKeyAccountBreakdownResponse } from '@/api'
import { usePluginPolling } from '@sdk/polling'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import {
  getUsageRecordInsightsDiagnostics,
  getUsageRecordInsightsKeyAccount,
  getUsageRecordInsightsOverview,
  getUsageRecords,
  getUsageRecordSummary,
} from '@/api'
import { withMinimumDuration } from '@/utils/async'

interface UseUsageRecordsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  active: Readonly<Ref<boolean>>
}

type UsageLoadScope = 'all' | 'table'

interface UsageLoadOptions {
  scope?: UsageLoadScope
  background?: boolean
}

export function useUsageRecordsTable(options: UseUsageRecordsTableOptions) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<UsageDisplayRecord[]>([])
  const summary = shallowRef(emptySummary())
  const insights = shallowRef(emptyInsights())
  const keyAccount = shallowRef(emptyKeyAccount())
  const currentPage = shallowRef(1)
  const pageSize = shallowRef(10)
  const totalRecords = shallowRef(0)
  const searchQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
  const quotaSearchQuery = shallowRef('')
  const quotaSearch = computed(() => quotaSearchQuery.value.trim() || undefined)
  const providerQuery = shallowRef('')
  let tableParams = snapshot()
  const refreshingList = shallowRef(false)
  const refreshingKeyAccount = shallowRef(false)
  const keyAccountLoading = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  let tableRequestId = 0
  let analyticsRequestId = 0
  let diagnosticRequestId = 0
  let keyAccountRequestId = 0
  let tableController: AbortController | undefined
  let analyticsController: AbortController | undefined
  let diagnosticController: AbortController | undefined
  let keyAccountController: AbortController | undefined
  let disposed = false
  let pending = 0
  const scopedParams = () => ({
    ...options.latestTimeRangeParams(),
    ...(providerQuery.value ? { provider: providerQuery.value } : {}),
  })
  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
  }))

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      provider: providerQuery.value || undefined,
      search: search.value,
    }
  }

  function resetPagination() {
    currentPage.value = 1
    totalRecords.value = 0
  }

  async function loadUsageRecords(loadOptions: UsageLoadOptions = {}) {
    const { scope = 'all', background = false } = loadOptions
    const globalParams = scopedParams()
    if (scope === 'all') {
      resetPagination()
      tableParams = snapshot()
    }

    await Promise.all([
      ...(options.active.value ? [loadUsagePage(background)] : []),
      ...(scope === 'all' ? [loadUsageAnalytics(globalParams, background)] : []),
      ...(scope === 'all' ? [loadKeyAccount(background)] : []),
    ])
  }

  async function loadUsagePage(background: boolean) {
    pending++
    const requestId = ++tableRequestId
    tableController?.abort()
    tableController = new AbortController()
    loading.value = !background
    try {
      const result = await getUsageRecords({
        currentPage: currentPage.value,
        pageSize: pageSize.value,
        ...tableParams,
      }, { signal: tableController.signal, silent: background })
      if (requestId !== tableRequestId)
        return

      records.value = result.items
      pageSize.value = result.pageSize
      totalRecords.value = result.total
      currentPage.value = result.currentPage
    }
    catch {}
    finally {
      pending--
      if (requestId === tableRequestId) {
        loading.value = false
      }
    }
  }

  async function loadUsageAnalytics(globalParams: ReturnType<typeof scopedParams>, background: boolean) {
    pending++
    const requestId = ++analyticsRequestId
    const diagnosticsId = ++diagnosticRequestId
    analyticsController?.abort()
    diagnosticController?.abort()
    analyticsController = new AbortController()
    const requestOptions = { signal: analyticsController.signal, silent: background }
    const dimension = diagnosticDimension.value
    analyticsLoading.value = !background
    try {
      const [nextSummary, overview, diagnostics] = await Promise.all([
        getUsageRecordSummary(globalParams, requestOptions),
        getUsageRecordInsightsOverview(globalParams, requestOptions),
        getUsageRecordInsightsDiagnostics({
          ...globalParams,
          dimension,
        }, requestOptions),
      ])
      if (requestId !== analyticsRequestId)
        return

      summary.value = nextSummary
      insights.value = {
        overview,
        diagnostics:
          diagnosticsId === diagnosticRequestId && dimension === diagnosticDimension.value
            ? diagnostics
            : insights.value.diagnostics,
      }
    }
    catch {}
    finally {
      pending--
      if (requestId === analyticsRequestId) {
        analyticsLoading.value = false
      }
    }
  }

  async function loadDiagnostics() {
    const requestId = ++diagnosticRequestId
    diagnosticController?.abort()
    diagnosticController = new AbortController()
    const dimension = diagnosticDimension.value
    const params = scopedParams()
    try {
      const diagnostics = await getUsageRecordInsightsDiagnostics({
        ...params,
        dimension,
      }, { signal: diagnosticController.signal })
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value)
        return
      insights.value = {
        ...insights.value,
        diagnostics,
      }
    }
    catch {}
  }
  async function loadKeyAccount(background = false) {
    // 筛选、时间范围、手动刷新和轮询共用同一请求序列，旧筛选结果不能覆盖新选择。
    pending++
    const requestId = ++keyAccountRequestId
    keyAccountController?.abort()
    keyAccountController = new AbortController()
    const params = {
      ...scopedParams(),
      ...(quotaSearch.value ? { search: quotaSearch.value } : {}),
    }
    keyAccountLoading.value = !background
    try {
      const result = await getUsageRecordInsightsKeyAccount(params, { signal: keyAccountController.signal, silent: background })
      if (requestId !== keyAccountRequestId || params.search !== quotaSearch.value)
        return
      keyAccount.value = result
    }
    catch {}
    finally {
      pending--
      if (requestId === keyAccountRequestId) {
        keyAccountLoading.value = false
      }
    }
  }

  async function refreshKeyAccount() {
    if (refreshingKeyAccount.value || keyAccountLoading.value)
      return
    refreshingKeyAccount.value = true
    try {
      await withMinimumDuration(() => loadKeyAccount(true))
    }
    finally {
      refreshingKeyAccount.value = false
    }
  }

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value)
      return
    refreshingList.value = true
    try {
      await withMinimumDuration(reloadLatestTable)
    }
    finally {
      refreshingList.value = false
    }
  }

  function reloadLatestTable() {
    tableParams = snapshot()
    resetPagination()
    return loadUsageRecords({ scope: 'table' })
  }

  function handlePageChange(nextPage: number) {
    if (tableParams.search !== search.value) {
      void reloadLatestTable()
      return
    }
    currentPage.value = nextPage
    void loadUsageRecords({ scope: 'table' })
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    if (tableParams.search !== search.value) {
      void reloadLatestTable()
      return
    }
    resetPagination()
    void loadUsageRecords({ scope: 'table' })
  }

  onMounted(() => {
    loadUsageRecords()
  })

  usePluginPolling(async () => {
    if (pending || refreshingList.value || tableParams.search !== search.value)
      return
    // 第一页追踪最新请求；后续页保留时间快照，避免新记录把正在查看的行挤走。
    if (currentPage.value === 1)
      tableParams = snapshot()
    await Promise.all([
      ...(options.active.value ? [loadUsagePage(true)] : []),
      loadUsageAnalytics(scopedParams(), true),
      loadKeyAccount(true),
    ])
  }, shallowRef(30_000))

  watch(diagnosticDimension, () => {
    void loadDiagnostics()
  })

  watch(providerQuery, () => {
    void loadUsageRecords({ background: true })
  })

  watch(options.active, (active) => {
    if (active) {
      void reloadLatestTable()
    }
    else {
      tableRequestId += 1
      tableController?.abort()
    }
  })

  watchDebounced(
    search,
    () => {
      if (!disposed && options.active.value && tableParams.search !== search.value)
        void reloadLatestTable()
    },
    { debounce: 250 },
  )
  watchDebounced(
    quotaSearch,
    () => {
      if (!disposed)
        void loadKeyAccount()
    },
    { debounce: 250 },
  )

  onScopeDispose(() => {
    disposed = true
    tableRequestId += 1
    analyticsRequestId += 1
    diagnosticRequestId += 1
    keyAccountRequestId += 1
    tableController?.abort()
    analyticsController?.abort()
    diagnosticController?.abort()
    keyAccountController?.abort()
  })

  return {
    currentPage,
    pageSize,
    searchQuery,
    quotaSearchQuery,
    providerQuery,
    usagePagination,
    loading,
    analyticsLoading,
    keyAccountLoading,
    records,
    summary,
    insights,
    keyAccount,
    refreshingList,
    refreshingKeyAccount,
    diagnosticDimension,
    loadUsageRecords,
    refreshUsageRecords,
    loadKeyAccount,
    refreshKeyAccount,
    handlePageChange,
    handlePageSizeChange,
  }
}

function emptySummary() {
  const summary: Awaited<ReturnType<typeof getUsageRecordSummary>> = {
    totalRequests: '0',
    inputTokens: '0',
    outputTokens: '0',
    cachedTokens: '0',
    cacheWriteTokens: '0',
    totalTokens: '0',
    averageLatencyMs: '0 ms',
  }
  return summary
}

function emptyInsights() {
  return {
    overview: emptyOverview(),
    diagnostics: emptyDiagnostics(),
  }
}

function emptyOverview() {
  const overview: Awaited<ReturnType<typeof getUsageRecordInsightsOverview>> = {
    granularity: '1d',
    health: {
      totalRequests: 0,
      successRequests: 0,
      failedRequests: 0,
      cancelledRequests: 0,
      incompleteRequests: 0,
      callerErrorRequests: 0,
      successRate: 0,
      completionRate: 0,
      requestChangeRate: null,
      successRateChange: null,
      points: [],
    },
    performance: {
      latencyP50Ms: null,
      latencyP95Ms: null,
      latencyP99Ms: null,
      firstTokenP50Ms: null,
      firstTokenP95Ms: null,
      firstTokenP99Ms: null,
      admissionDecisionP50Ms: null,
      admissionDecisionP95Ms: null,
      accountSelectionWaitP50Ms: null,
      accountSelectionWaitP95Ms: null,
      outputThroughputP10: null,
      outputThroughputP50: null,
      outputThroughputP90: null,
      capacityUtilization: null,
      capacityUtilizationP95: null,
      latencyCoverage: 0,
      firstTokenCoverage: 0,
      admissionDecisionCoverage: 0,
      accountSelectionWaitCoverage: 0,
      capacityCoverage: 0,
      points: [],
    },
    cost: {
      estimatedCost: null,
      standardCost: null,
      noCacheCost: null,
      cacheSavings: null,
      tierPremium: null,
      costPerRequest: null,
      costPerSuccessfulRequest: null,
      tokensPerRequest: 0,
      cachedTokenRate: 0,
      cacheHitRequestRate: 0,
      inputTokens: 0,
      outputTokens: 0,
      cachedTokens: 0,
      totalTokens: 0,
      points: [],
      coverage: { known: 0, partial: 0, unknown: 0, notBillable: 0 },
    },
  }
  return overview
}

function emptyDiagnostics() {
  const diagnostics: Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>> = {
    dimension: 'model',
    items: [],
  }
  return diagnostics
}

function emptyKeyAccount(): UsageKeyAccountBreakdownResponse {
  return { items: [] }
}

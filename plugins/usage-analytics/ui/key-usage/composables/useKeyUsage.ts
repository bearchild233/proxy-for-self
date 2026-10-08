import type { KeyUsageOverview, KeyUsageRecordKind } from '@/api/modules/key-usage'
import { usePluginPolling } from '@sdk/polling'
import { refDebounced } from '@vueuse/core'
import dayjs from 'dayjs'
import { computed, shallowRef, watch } from 'vue'
import { getKeyUsageOverview, getKeyUsageRecords } from '@/api/modules/key-usage'
import { useRequestState } from '@/composables/useRequestState'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { chinaTimeRange } from '@/utils/date'

export function useKeyUsage() {
  const period = shallowRef('today')
  const model = shallowRef('')
  const selectedModel = refDebounced(model, 300)
  const kind = shallowRef<KeyUsageRecordKind>('success')
  const refreshInterval = shallowRef('30')
  const overview = shallowRef<KeyUsageOverview>()
  const refreshing = shallowRef(false)
  const recordsStale = shallowRef(false)
  const overviewRequest = useRequestState()
  let queryGeneration = 0
  const rangeEnd = shallowRef(dayjs())
  const recordsEnd = shallowRef(rangeEnd.value)
  const query = computed(() => {
    const days = period.value === '7d' ? 6 : period.value === '30d' ? 29 : 0
    return {
      ...chinaTimeRange(days, rangeEnd.value),
      model: selectedModel.value.trim() || undefined,
    }
  })
  const records = useStablePagedQuery({
    initialPageSize: 20,
    load: (pagination, options) => getKeyUsageRecords({ ...query.value, ...chinaTimeRange(period.value === '7d' ? 6 : period.value === '30d' ? 29 : 0, recordsEnd.value), ...pagination, kind: kind.value }, { ...options, silent: true }),
    onSuccess: () => {
      recordsStale.value = false
      records.error.value = ''
    },
  })

  async function loadOverview(background = false) {
    const id = overviewRequest.start(background)
    try {
      const result = await getKeyUsageOverview(query.value, { signal: overviewRequest.signal, silent: true })
      if (overviewRequest.isCurrent(id))
        overview.value = result
    }
    catch (cause) {
      overviewRequest.fail(id, cause, background)
    }
    finally {
      overviewRequest.finish(id)
    }
  }

  async function refresh() {
    if (refreshing.value || overviewRequest.loading.value || records.pending.value)
      return
    refreshing.value = true
    const generation = queryGeneration
    rangeEnd.value = dayjs()
    if (records.currentPage.value === 1)
      recordsEnd.value = rangeEnd.value
    try {
      const [, recordsOk] = await Promise.all([loadOverview(true), records.execute(undefined, { silent: true })])
      // 轮询被新筛选或翻页取代时，不把取消结果标成刷新失败。
      if (generation === queryGeneration)
        recordsStale.value = !recordsOk
    }
    finally {
      refreshing.value = false
    }
  }

  watch([period, selectedModel], () => {
    queryGeneration += 1
    rangeEnd.value = dayjs()
    recordsEnd.value = rangeEnd.value
    overview.value = undefined
    records.items.value = []
    void loadOverview()
    void records.reloadFromStart()
  }, { immediate: true })

  watch(kind, () => {
    recordsEnd.value = dayjs()
    queryGeneration += 1
    records.items.value = []
    void records.reloadFromStart()
  })

  usePluginPolling(refresh, computed(() => Number(refreshInterval.value) * 1000))

  function changePage(page: number) {
    queryGeneration += 1
    void records.execute(page)
  }

  function changePageSize(size: number) {
    queryGeneration += 1
    records.pageSize.value = size
    void records.reloadFromStart()
  }

  return {
    period,
    model,
    kind,
    refreshInterval,
    overview,
    refreshing,
    recordsStale,
    overviewLoading: overviewRequest.loading,
    overviewError: overviewRequest.error,
    records,
    refresh,
    changePageSize,
    changePage,
  }
}

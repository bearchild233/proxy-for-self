import type { Account } from '@/api/modules/accounts'
import type { WarmupModel, WarmupRead, WarmupRun, WarmupTask } from '@/api/modules/accountWarmup'
import { usePluginPolling } from '@sdk/polling'
import { computed, onMounted, onScopeDispose, ref, shallowRef } from 'vue'
import { getAccounts } from '@/api/modules/accounts'
import { accountWarmupHistory, cancelAccountWarmup, configureAccountWarmup, readAccountWarmup, warmupRequestError } from '@/api/modules/accountWarmup'
import { toast } from '@/components/base/BaseToast'
import { cloneSettings, defaultWarmupSettings, settingsError } from './model'

export async function loadAccountNames(signal?: AbortSignal) {
  const names = new Map<string, Pick<Account, 'id' | 'name' | 'provider'>>()
  const first = await getAccounts({ page: 1, pageSize: 200, archived: false }, { silent: true, signal })
  for (const item of first.items)
    names.set(item.id, item)
  for (let page = 2; page <= first.page.totalPages; page++) {
    signal?.throwIfAborted()
    const result = await getAccounts({ page, pageSize: 200, archived: false }, { silent: true, signal })
    for (const item of result.items)
      names.set(item.id, item)
  }
  return names
}

export function useWarmupSettings() {
  const view = shallowRef<WarmupRead>()
  const draft = ref(defaultWarmupSettings())
  const baseline = shallowRef('')
  const revision = shallowRef(0)
  const error = shallowRef('')
  const namesError = shallowRef('')
  const saveError = shallowRef('')
  const changedElsewhere = shallowRef(false)
  const loading = shallowRef(false)
  const saving = shallowRef(false)
  const names = shallowRef(new Map<string, Pick<Account, 'id' | 'name' | 'provider'>>())
  const runs = shallowRef<WarmupRun[]>([])
  const items = shallowRef<WarmupTask[]>([])
  const selectedRunId = shallowRef('')
  const historyError = shallowRef('')
  const cancelling = shallowRef(false)
  const dirty = computed(() => baseline.value !== '' && JSON.stringify(draft.value) !== baseline.value)
  const validation = computed(() => {
    const value = draft.value
    if (view.value?.available && !Array.isArray(view.value.settings.groupIds))
      return '请升级自动预热插件后保存新设置，当前草稿已保留'
    if (value.targetMode === 'groups' && (!view.value?.available || view.value.groupTargetsSupported !== true))
      return '当前服务尚未确认动态分组能力，请刷新或升级后保存分组计划'
    const requestError = warmupRequestError({ model: value.defaultModel, prompt: value.prompt, reasoningEffort: value.reasoningEffort })
    if (requestError)
      return requestError
    for (const item of value.accountOverrides) {
      const overrideError = warmupRequestError({ model: item.model ?? value.defaultModel, prompt: value.prompt, reasoningEffort: item.reasoningEffort ?? value.reasoningEffort })
      if (overrideError)
        return `账号独立设置：${overrideError}`
    }
    return settingsError(value)
  })
  const models = ref<Record<string, WarmupModel[]>>({})
  const modelLoading = ref(new Set<string>())
  const modelErrors = ref<Record<string, string>>({})
  const modelSequence = new Map<string, number>()
  let readSequence = 0
  let historySequence = 0
  let disposed = false
  const controller = new AbortController()

  function apply(next: WarmupRead, replace = false) {
    view.value = next
    if (!next.available)
      return
    if (replace || (!dirty.value && !saving.value)) {
      const normalized = cloneSettings(next.settings)
      draft.value = normalized
      baseline.value = JSON.stringify(normalized)
      revision.value = next.revision
      changedElsewhere.value = false
    }
    else if (next.revision !== revision.value) {
      changedElsewhere.value = true
    }
  }
  async function refresh(replace = false, background = false) {
    const sequence = ++readSequence
    if (!background)
      loading.value = true
    try {
      const next = await readAccountWarmup()
      if (sequence !== readSequence || disposed)
        return
      apply(next, replace)
      error.value = ''
    }
    catch (cause) {
      if (sequence === readSequence && !disposed)
        error.value = cause instanceof Error ? cause.message : '预热服务读取失败'
    }
    finally {
      if (sequence === readSequence)
        loading.value = false
    }
  }
  async function refreshNames() {
    try {
      const next = await loadAccountNames(controller.signal)
      if (!disposed) {
        names.value = next
        namesError.value = ''
      }
    }
    catch {
      if (!disposed)
        namesError.value = '账号名称读取失败，当前以账号 ID 展示'
    }
  }
  async function save() {
    if (saving.value || validation.value || !view.value?.available)
      return
    const snapshot = cloneSettings(draft.value)
    const expected = revision.value
    readSequence++
    saving.value = true
    saveError.value = ''
    try {
      const next = await configureAccountWarmup(expected, snapshot)
      if (disposed)
        return
      apply(next, true)
      toast.success('预热设置已保存')
    }
    catch (cause) {
      saveError.value = cause instanceof Error ? cause.message : '保存失败'
      if (/revision|版本|冲突/i.test(saveError.value))
        changedElsewhere.value = true
    }
    finally {
      // 保存期间启动的轮询也失效，避免旧配置在保存完成后覆盖刚保存的表单。
      readSequence++
      saving.value = false
    }
  }
  async function refreshHistory() {
    const sequence = ++historySequence
    try {
      const result = await accountWarmupHistory({ limit: 20, source: 'automatic' })
      if (sequence !== historySequence || disposed)
        return
      // 服务在限量前过滤来源，客户端再守住自动化页面的展示与取消边界。
      const automatic = result.runs.filter(item => item.source === 'daily' || item.source === 'reset')
      runs.value = automatic
      if (!selectedRunId.value || !automatic.some(item => item.id === selectedRunId.value))
        selectedRunId.value = automatic[0]?.id ?? ''
      if (selectedRunId.value) {
        const runId = selectedRunId.value
        const details = await accountWarmupHistory({ runId, source: 'automatic' })
        if (sequence !== historySequence || disposed)
          return
        if (!details.runs.some(item => item.id === runId && (item.source === 'daily' || item.source === 'reset'))) {
          items.value = []
          throw new Error('自动预热批次来源不匹配，请刷新记录')
        }
        // 自动批次可复用已存在的手动任务，来源边界由批次守住，不能丢掉合并任务。
        items.value = details.items
      }
      else {
        items.value = []
      }
      historyError.value = ''
    }
    catch (cause) {
      if (sequence === historySequence && !disposed)
        historyError.value = cause instanceof Error ? cause.message : '任务记录读取失败'
    }
  }
  function selectRun(id: string) {
    if (!runs.value.some(item => item.id === id && (item.source === 'daily' || item.source === 'reset')))
      return
    selectedRunId.value = id
    items.value = []
    void refreshHistory()
  }
  async function cancel() {
    const selected = runs.value.find(item => item.id === selectedRunId.value)
    if (!selected || !['daily', 'reset'].includes(selected.source) || !['queued', 'running'].includes(selected.status) || selected.pending <= 0 || cancelling.value)
      return
    cancelling.value = true
    try {
      await cancelAccountWarmup(selectedRunId.value)
      await refreshHistory()
    }
    catch (cause) { historyError.value = cause instanceof Error ? cause.message : '停止失败' }
    finally { cancelling.value = false }
  }
  async function loadModels(accountId: string, model?: string) {
    const sequence = (modelSequence.get(accountId) ?? 0) + 1
    modelSequence.set(accountId, sequence)
    modelLoading.value.add(accountId)
    try {
      const result = await readAccountWarmup({ accountIds: [accountId], includeModels: true, model })
      if (modelSequence.get(accountId) !== sequence || disposed)
        return
      if (!result.available)
        throw new Error(result.reason === 'plugin_disabled' ? '预热插件未启用' : '预热服务尚未就绪')
      models.value[accountId] = result.accounts.find(item => item.id === accountId)?.models ?? []
      modelErrors.value[accountId] = ''
    }
    catch (cause) {
      if (modelSequence.get(accountId) === sequence && !disposed)
        modelErrors.value[accountId] = cause instanceof Error ? cause.message : '模型目录读取失败'
    }
    finally {
      if (modelSequence.get(accountId) === sequence)
        modelLoading.value.delete(accountId)
    }
  }
  async function refreshAll() {
    await Promise.all([refresh(false, true), refreshHistory()])
  }
  onMounted(() => {
    void refresh()
    void refreshNames()
    void refreshHistory()
  })
  usePluginPolling(refreshAll, shallowRef(15_000))
  onScopeDispose(() => {
    disposed = true
    readSequence++
    historySequence++
    controller.abort()
  })
  return { view, draft, dirty, validation, revision, error, namesError, saveError, changedElsewhere, loading, saving, names, runs, items, selectedRunId, historyError, cancelling, models, modelLoading, modelErrors, refresh, refreshNames, save, refreshHistory, selectRun, cancel, loadModels }
}

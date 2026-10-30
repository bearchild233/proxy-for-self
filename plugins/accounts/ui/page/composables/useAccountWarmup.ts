import type { Ref } from 'vue'
import type { AccountRow } from '../constants'
import type { WarmupModel, WarmupRead, WarmupRequest, WarmupRun, WarmupRunInput } from '@/api/modules/accountWarmup'
import { enabled } from '@sdk/catalog'
import { usePluginPolling } from '@sdk/polling'
import { computed, onMounted, onScopeDispose, ref, shallowRef, watch } from 'vue'
import { readAccountWarmup, runAccountWarmup, warmupRequestError } from '@/api/modules/accountWarmup'

export function useAccountWarmup(archived: Ref<boolean>) {
  const view = shallowRef<WarmupRead>()
  const error = ref('')
  const submitting = ref(false)
  const result = shallowRef<WarmupRun>()
  const installed = computed(() => enabled('account-warmup'))
  const compatible = computed(() => view.value?.available === true
    && typeof view.value.settings.prompt === 'string'
    && typeof view.value.settings.reasoningEffort === 'string'
    && view.value.accountStates !== undefined)
  const ready = computed(() => installed.value && !error.value && compatible.value && view.value?.available === true && view.value.runtime.ready)
  const dialogOpen = ref(false)
  const targetIds = shallowRef<string[]>()
  const targetName = ref('全部当前账号')
  const request = ref<WarmupRequest>({ model: '', prompt: 'hi', reasoningEffort: 'auto' })
  const submitError = ref('')
  const pendingSubmission = shallowRef<WarmupRunInput>()
  const requestLocked = computed(() => submitting.value || pendingSubmission.value !== undefined)
  const modelAccountId = ref('')
  const modelCatalog = ref<Record<string, WarmupModel[]>>({})
  const models = computed(() => modelCatalog.value[modelAccountId.value] ?? [])
  const modelsLoading = ref(false)
  const modelsError = ref('')
  const status = computed(() => {
    if (archived.value)
      return '归档账号不参与预热'
    if (!installed.value || (view.value?.available === false && view.value.reason === 'plugin_disabled'))
      return '预热插件未安装或未启用'
    if (error.value)
      return '预热状态读取失败，请刷新'
    if (view.value?.available && !compatible.value)
      return '请升级预热插件，以使用账号状态、提示词和强度设置'
    if (!ready.value)
      return '预热服务尚未就绪'
    const run = result.value ?? (view.value?.available ? view.value.runtime.lastRun : null)
    return run ? `${run.status === 'completed' ? '最近预热已结束' : run.status === 'cancelled' ? '最近预热已停止' : '已加入预热队列'}：受理 ${run.accepted} / ${run.total}，失败 ${run.failed}，跳过 ${run.skipped}，未知 ${run.unknown}` : '手动预热可设置本次提示词、模型和推理强度'
  })
  let sequence = 0
  let modelSequence = 0
  let disposed = false

  async function refresh() {
    const current = ++sequence
    if (!installed.value) {
      view.value = undefined
      return
    }
    try {
      const next = await readAccountWarmup()
      if (current !== sequence || disposed)
        return
      view.value = next
      if (next.available && next.runtime.lastRun)
        result.value = next.runtime.lastRun.id === result.value?.id ? next.runtime.lastRun : undefined
      error.value = ''
    }
    catch (cause) {
      if (current === sequence && !disposed)
        error.value = cause instanceof Error ? cause.message : '预热状态读取失败'
    }
  }
  async function loadModels(id: string) {
    const current = ++modelSequence
    modelAccountId.value = id
    modelsError.value = ''
    modelsLoading.value = false
    if (!id)
      return
    modelsLoading.value = true
    try {
      const next = await readAccountWarmup({ accountIds: [id], includeModels: true })
      if (current !== modelSequence || disposed)
        return
      if (!next.available)
        throw new Error('预热模型目录暂不可用')
      modelCatalog.value[id] = next.accounts.find(item => item.id === id)?.models ?? []
    }
    catch (cause) {
      if (current === modelSequence && !disposed)
        modelsError.value = cause instanceof Error ? cause.message : '模型目录读取失败'
    }
    finally {
      if (current === modelSequence)
        modelsLoading.value = false
    }
  }
  function canRun(account: AccountRow) {
    const state = view.value?.available ? view.value.accountStates?.[account.id] : undefined
    return ready.value && !submitting.value && !archived.value && !account.lifecycle.archived
      && account.provider === 'openai' && account.authenticationKind === 'oauth'
      && account.status === 'normal' && !account.quotaProtectionReached
      && !['queued', 'running'].includes(state?.status ?? '')
  }
  function open(ids?: string[], name?: string) {
    if (!ready.value || archived.value || submitting.value)
      return
    // 未知的提交先回读同一个 submission，不能因关闭弹窗或改变范围丢失幂等标识。
    if (!pendingSubmission.value) {
      const unique = ids ? [...new Set(ids)] : undefined
      if (unique && (!unique.length || unique.length > 500)) {
        submitError.value = '请选择 1 至 500 个账号'
        return
      }
      targetIds.value = unique
      targetName.value = name ?? (unique ? `选中 ${unique.length} 个账号` : '全部当前账号')
      const settings = view.value?.available ? view.value.settings : undefined
      const override = unique?.length === 1 ? settings?.accountOverrides.find(item => item.accountId === unique[0]) : undefined
      request.value = {
        model: override?.model ?? settings?.defaultModel ?? '',
        prompt: settings?.prompt ?? 'hi',
        reasoningEffort: override?.reasoningEffort ?? settings?.reasoningEffort ?? 'auto',
      }
      submitError.value = ''
    }
    dialogOpen.value = true
    const catalog = targetIds.value?.[0] ?? (view.value?.available ? view.value.accounts.find(item => item.supported && item.enabled)?.id : undefined)
    void loadModels(catalog ?? '')
  }
  function openAccount(account: AccountRow) {
    if (canRun(account))
      open([account.id], account.name || account.email || account.id)
  }
  async function submit() {
    if (!ready.value || archived.value || submitting.value)
      return
    if (!pendingSubmission.value) {
      submitError.value = warmupRequestError(request.value)
      if (submitError.value)
        return
      const model = models.value.find(item => item.id === request.value.model)
      if (request.value.reasoningEffort !== 'auto' && model?.reasoningEfforts && !model.reasoningEfforts.includes(request.value.reasoningEffort)) {
        submitError.value = '所选模型不支持此推理强度，请重新选择'
        return
      }
      pendingSubmission.value = {
        submissionId: crypto.randomUUID(),
        scope: targetIds.value ? 'selected' : 'all',
        ...(targetIds.value ? { accountIds: [...targetIds.value] } : {}),
        ...request.value,
      }
    }
    submitting.value = true
    submitError.value = ''
    try {
      const next = await runAccountWarmup(pendingSubmission.value)
      if (disposed)
        return
      result.value = next.run
      pendingSubmission.value = undefined
      dialogOpen.value = false
      await refresh()
    }
    catch (cause) {
      if (!disposed)
        submitError.value = `${cause instanceof Error ? cause.message : '预热提交失败'}。再次点击将核对同一批提交。`
    }
    finally { submitting.value = false }
  }
  onMounted(refresh)
  watch(installed, refresh)
  usePluginPolling(refresh, shallowRef(15_000))
  onScopeDispose(() => {
    disposed = true
    sequence++
    modelSequence++
  })
  return { view, installed, ready, status, error, result, dialogOpen, request, requestLocked, targetName, submitError, pendingSubmission, submitting, modelAccountId, models, modelsLoading, modelsError, refresh, loadModels, canRun, open, openAccount, submit }
}

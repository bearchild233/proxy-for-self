import type { Ref } from 'vue'
import type { AccountModelAccess, ApiKeyConfiguration, getAccounts } from '@/api'

import { computed, ref, shallowRef, watch } from 'vue'
import { batchUpdateAccounts, getAccountDetail, updateAccount, updateAccountApiKey } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError } from '../utils/modelAccess'
import { concurrencyLimitInput, parseAccountSchedulingForm } from '../utils/schedulingForm'
import { apiKeyAccountError, emptyApiKeyAccountForm } from '../utils/upstreamApiKey'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountEditor(options: {
  accounts: Ref<AccountRow[]>
  reloadAccounts: (options?: { background?: boolean }) => Promise<unknown>
  reloadGroups: () => Promise<unknown>
}) {
  const showEditModal = shallowRef(false)
  const editingAccountId = shallowRef<string | null>(null)
  const name = shallowRef('')
  const notes = shallowRef('')
  const schedulingEnabled = shallowRef(true)
  const concurrencyLimit = shallowRef('')
  const weight = shallowRef('1')
  const modelAccess = ref<AccountModelAccess | undefined>()
  const proxyMode = shallowRef('preserve')
  const proxyId = shallowRef('')
  const selectedGroupIds = ref<string[]>([])
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const apiKey = ref(emptyApiKeyAccountForm())
  const configurationRequest = useRequestState()
  const configurationLoading = configurationRequest.loading
  const configurationReady = shallowRef(false)
  const savedConfiguration = shallowRef<ApiKeyConfiguration>()
  let originalSettings = ''
  let originalGroups = ''

  function settingsFingerprint() {
    return JSON.stringify({
      name: name.value.trim(),
      notes: notes.value.trim(),
      enabled: schedulingEnabled.value,
      concurrencyLimit: concurrencyLimit.value.trim() === '' ? null : Number(concurrencyLimit.value),
      weight: Number(weight.value),
      modelAccess: { mode: modelAccess.value?.mode ?? 'all', models: [...new Set(modelAccess.value?.models ?? [])].sort() },
    })
  }

  function groupsFingerprint() {
    return JSON.stringify([...new Set(selectedGroupIds.value)].sort())
  }

  async function loadConfiguration(accountId: string) {
    const requestId = configurationRequest.start()
    try {
      const detail = await getAccountDetail({ accountId }, { signal: configurationRequest.signal })
      if (!configurationRequest.isCurrent(requestId))
        return
      if (!detail.credentialConfiguration)
        throw new Error('该账号没有 API Key 上游设置')
      apiKey.value = { ...emptyApiKeyAccountForm(), ...detail.credentialConfiguration }
      savedConfiguration.value = detail.credentialConfiguration
      configurationReady.value = true
    }
    catch (error) {
      configurationRequest.fail(requestId, error)
    }
    finally {
      configurationRequest.finish(requestId)
    }
  }

  const editingAccount = computed(() => {
    const accountId = editingAccountId.value
    return accountId
      ? options.accounts.value.find(account => account.id === accountId) ?? null
      : null
  })

  function open(account: AccountRow) {
    configurationRequest.invalidate()
    editingAccountId.value = account.id
    name.value = account.name
    notes.value = account.notes ?? ''
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = account.enabled
    concurrencyLimit.value = concurrencyLimitInput(account.concurrencyLimit)
    weight.value = String(account.weight)
    modelAccess.value = { ...account.modelAccess, models: [...account.modelAccess.models] }
    selectedGroupIds.value = account.groups.map(group => group.id)
    // 冻结打开时的表单，后台同步不能改变本次编辑的变更判断。
    originalSettings = settingsFingerprint()
    originalGroups = groupsFingerprint()
    apiKey.value = emptyApiKeyAccountForm()
    savedConfiguration.value = undefined
    configurationReady.value = false
    showEditModal.value = true
    if (account.authenticationKind === 'api_key')
      void loadConfiguration(account.id)
  }

  async function save() {
    const accountId = editingAccountId.value
    if (!accountId || saving.value)
      return
    if (!name.value.trim() || name.value.trim().length > 100) {
      toast.warning('请输入 1 至 100 字的账号名称')
      return
    }
    const isApiKey = editingAccount.value?.authenticationKind === 'api_key'
    if (isApiKey) {
      if (!configurationReady.value)
        return
      const error = apiKeyAccountError(apiKey.value, true)
      if (error) {
        toast.warning(error)
        return
      }
    }
    const modelError = accountModelAccessError(modelAccess.value)
    if (modelError) {
      toast.warning(modelError)
      return
    }
    const scheduling = parseAccountSchedulingForm(concurrencyLimit.value, weight.value)
    if (proxyMode.value === 'proxy' && !proxyId.value.trim()) {
      toast.warning('请选择已通过测试的代理')
      return
    }
    if (!scheduling.valid) {
      toast.warning(scheduling.message)
      return
    }

    const groupsChanged = groupsFingerprint() !== originalGroups
    const settingsChanged = settingsFingerprint() !== originalSettings || proxyMode.value !== 'preserve'
    const connectionChanged = isApiKey && (
      apiKey.value.apiKey !== ''
      || apiKey.value.base_url.trim() !== savedConfiguration.value?.base_url
      || apiKey.value.transport !== savedConfiguration.value?.transport
    )
    if (!settingsChanged && !groupsChanged && !connectionChanged) {
      showEditModal.value = false
      return
    }

    await saveAction.run(async () => {
      const settings = {
        name: name.value.trim(),
        accountId,
        notes: notes.value,
        outboundProxyId: proxyMode.value === 'preserve' ? undefined : proxyMode.value === 'direct' ? '' : proxyId.value.trim(),
        enabled: schedulingEnabled.value,
        concurrencyLimit: scheduling.values.concurrencyLimit,
        weight: scheduling.values.weight,
        modelAccess: modelAccess.value,
        groupIds: [...new Set(selectedGroupIds.value)],
      }
      if (connectionChanged) {
        await updateAccountApiKey({ accountId, baseUrl: apiKey.value.base_url.trim(), transport: apiKey.value.transport, apiKey: apiKey.value.apiKey || undefined, settings })
      }
      else if (!settingsChanged) {
        // 只写分组，保留后台或另一管理员同时修改的调度与账号资料。
        await batchUpdateAccounts({ accountIds: [accountId], groupIds: settings.groupIds })
      }
      else {
        await updateAccount(settings)
      }
      showEditModal.value = false
      toast.success('账号已更新')
      // 保存已经提交；回读保留当前表格，名称等编辑不需要重载整个分组目录。
      void Promise.all([options.reloadAccounts({ background: true }), ...(groupsChanged ? [options.reloadGroups()] : [])])
        .catch(() => toast.warning('账号已保存，列表同步失败，请刷新'))
    })
  }

  watch([showEditModal, saving], ([open, isSaving]) => {
    if (open || isSaving)
      return
    configurationRequest.invalidate()
    apiKey.value = emptyApiKeyAccountForm()
    savedConfiguration.value = undefined
    configurationReady.value = false
    editingAccountId.value = null
    notes.value = ''
    proxyMode.value = 'preserve'
    proxyId.value = ''
    schedulingEnabled.value = true
    concurrencyLimit.value = ''
    weight.value = '1'
    modelAccess.value = undefined
    selectedGroupIds.value = []
    originalSettings = ''
    originalGroups = ''
  })

  return {
    apiKey,
    configurationLoading,
    configurationReady,
    showEditModal,
    editingAccount,
    name,
    notes,
    schedulingEnabled,
    concurrencyLimit,
    weight,
    modelAccess,
    proxyMode,
    proxyId,
    selectedGroupIds,
    saving,
    open,
    save,
  }
}

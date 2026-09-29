import type { Ref } from 'vue'
import type { getApiKeys } from '@/api'
import type { ClientProfileSelection, XaiClientProfileSelection } from '@/api/modules/client-profiles'
import { ref, shallowRef, watch } from 'vue'
import {
  createBoundApiKey,
  deleteApiKey,
  disableApiKey,
  enableApiKey,
  getApiKeyBindings,
  revealApiKey,
  setApiKeyBinding,
  updateApiKey,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { useIdSet } from '@/composables/useIdSet'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number]

export interface ApiKeyFormValue {
  scopeMode: 'account' | 'accounts' | 'groups'
  accountIds: string[]
  rotationStrategy: string
  accountId: string
  excelBridgeEnabled: boolean
  openaiClientProfileOverride: ClientProfileSelection | null
  xaiClientProfileOverride: XaiClientProfileSelection | null
  customKey: string
  name: string
  label: string
  groupIds: string[]
  maxConcurrency: string
  requestsPerMinute: string
  dailyLimitUsd: string
  weeklyLimitUsd: string
}

export function useApiKeyMutations(options: {
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const copyText = useCopyText()
  const showFormModal = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const showSingleDeleteModal = shallowRef(false)
  const showKeyModal = shallowRef(false)
  const showAllAccountsConfirm = shallowRef(false)
  const createdKey = shallowRef('')
  const createdKeyName = shallowRef('')
  const createdKeyExcelEnabled = shallowRef(false)
  const editingKey = shallowRef<ApiKeyRow | null>(null)
  const pendingDeleteKey = shallowRef<ApiKeyRow | null>(null)
  const savingKeyAction = useAsyncAction()
  const deletingKeyAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const updatingStatusKeys = useIdSet<string>()
  const revealingKeys = useIdSet<string>()
  const savingKey = savingKeyAction.loading
  const deletingKey = deletingKeyAction.loading
  const batchDeleting = batchDeletingAction.loading
  const updatingStatusKeyIds = updatingStatusKeys.ids
  const revealingKeyIds = revealingKeys.ids
  const form = ref<ApiKeyFormValue>(emptyForm())

  function openCreate() {
    editingKey.value = null
    form.value = emptyForm()
    showFormModal.value = true
  }

  function openEdit(key: ApiKeyRow) {
    editingKey.value = key
    form.value = {
      scopeMode: key.routing?.mode ?? 'account',
      accountIds: [...(key.routing?.accountIds ?? (key.boundAccountId ? [key.boundAccountId] : []))],
      rotationStrategy: key.routing?.rotationStrategy ?? '',
      accountId: key.boundAccountId ?? '',
      excelBridgeEnabled: key.excelBridgeEnabled ?? false,
      openaiClientProfileOverride: key.openaiClientProfileOverride ? { ...key.openaiClientProfileOverride } : null,
      xaiClientProfileOverride: key.xaiClientProfileOverride ? { ...key.xaiClientProfileOverride } : null,
      customKey: '',
      name: key.name,
      label: key.label ?? '',
      groupIds: [...(key.routing?.groupIds ?? [])],
      maxConcurrency: limitInputValue(key.maxConcurrency),
      requestsPerMinute: limitInputValue(key.requestsPerMinute),
      dailyLimitUsd: limitInputValue(key.dailyLimitUsd),
      weeklyLimitUsd: limitInputValue(key.weeklyLimitUsd),
    }
    showFormModal.value = true
  }

  function requestSave() {
    if (!validateForm() || savingKey.value)
      return
    void save()
  }

  async function confirmAllAccountsScope() {
    showAllAccountsConfirm.value = false
    await save()
  }

  async function save() {
    if (!validateForm() || savingKey.value)
      return

    await savingKeyAction.run(
      async () => {
        const payload = {
          openaiClientProfileOverride: null,
          xaiClientProfileOverride: null,
          name: form.value.name.trim(),
          label: form.value.label.trim() || null,
          groupIds: [],
          maxConcurrency: parseLimit(form.value.maxConcurrency),
          requestsPerMinute: parseLimit(form.value.requestsPerMinute),
          dailyLimitUsd: form.value.dailyLimitUsd.trim() || '0',
          weeklyLimitUsd: form.value.weeklyLimitUsd.trim() || '0',
        }
        const routing = {
          mode: form.value.scopeMode,
          accountIds: form.value.scopeMode === 'account' ? [form.value.accountId] : form.value.scopeMode === 'accounts' ? [...new Set(form.value.accountIds)] : [],
          groupIds: form.value.scopeMode === 'groups' ? [...new Set(form.value.groupIds)] : [],
          rotationStrategy: form.value.rotationStrategy || null,
        }
        const accountId = routing.mode === 'account' ? form.value.accountId : null
        const current = editingKey.value
        if (current) {
          const before = await getApiKeyBindings([current.id])
          const oldBinding = before.items.find(item => item.id === current.id)
          if (JSON.stringify(oldBinding?.routing ?? null) !== JSON.stringify(current.routing ?? null)
            || (oldBinding?.accountId ?? null) !== (current.boundAccountId ?? null)
            || (oldBinding?.excelBridgeEnabled ?? false) !== (current.excelBridgeEnabled ?? false)) {
            throw new Error('账号或插件绑定已被其他操作修改，请刷新后重试')
          }
          await updateApiKey({ id: current.id, ...payload })
          const latest = await getApiKeyBindings([current.id])
          const binding = latest.items.find(item => item.id === current.id)
          if (!binding || JSON.stringify(binding.routing ?? null) !== JSON.stringify(oldBinding?.routing ?? null) || binding.accountId !== oldBinding?.accountId
            || binding.excelBridgeEnabled !== oldBinding?.excelBridgeEnabled) {
            throw new Error('基础信息已更新，但绑定发生变化，请刷新后确认')
          }
          const previousRouting = binding.routing ?? { mode: 'account', accountIds: binding.accountId ? [binding.accountId] : [], groupIds: [], rotationStrategy: null }
          if (JSON.stringify(previousRouting) !== JSON.stringify(routing) || binding.excelBridgeEnabled !== form.value.excelBridgeEnabled)
            await setApiKeyBinding({ id: current.id, accountId, routing, excelBridgeEnabled: form.value.excelBridgeEnabled, expectedRevision: latest.configRevision })
        }
        else {
          const result = await createBoundApiKey({
            key: { ...payload, customKey: form.value.customKey || undefined },
            accountId,
            routing,
            excelBridgeEnabled: form.value.excelBridgeEnabled,
          })
          createdKey.value = result.plaintextKey
          createdKeyName.value = payload.name
          createdKeyExcelEnabled.value = form.value.excelBridgeEnabled
        }

        showFormModal.value = false
        editingKey.value = null
        form.value = emptyForm()
        await options.reload()
        if (current) {
          toast.success('API Key 已更新')
        }
        else {
          showKeyModal.value = true
          toast.success('API Key 创建成功')
        }
      },
      { onError: () => void options.reload() },
    )
  }

  function validateForm() {
    if ((form.value.scopeMode === 'account' && !form.value.accountId) || (form.value.scopeMode === 'accounts' && !form.value.accountIds.length) || (form.value.scopeMode === 'groups' && !form.value.groupIds.length)) {
      toast.warning('请选择账号或账号组')
      return false
    }
    for (const [label, value] of [['日限额', form.value.dailyLimitUsd], ['周限额', form.value.weeklyLimitUsd]]) {
      if (value.trim() && !/^\d{1,10}(?:\.\d{1,10})?$/.test(value.trim())) {
        toast.warning(`${label}必须是非负金额，最多 10 位小数`)
        return false
      }
    }
    if (!form.value.name.trim()) {
      toast.warning('请输入 API Key 名称')
      return false
    }
    if (!editingKey.value && form.value.customKey && !/^[\x21-\x7E]+$/.test(form.value.customKey)) {
      toast.warning('自定义 Key 只能包含 HTTP 可传输的可见字符，不能包含空格或换行')
      return false
    }
    for (const [label, value] of [
      ['最大并发', form.value.maxConcurrency],
      ['每分钟请求数', form.value.requestsPerMinute],
    ] as const) {
      const parsed = Number(value)
      if (!Number.isSafeInteger(parsed) || parsed < 0) {
        toast.warning(`${label}必须是非负整数`)
        return false
      }
    }
    return true
  }

  function requestDeleteKey(key: ApiKeyRow) {
    pendingDeleteKey.value = key
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    if (deletingKey.value)
      return
    const keyId = pendingDeleteKey.value?.id
    if (!keyId)
      return

    await deletingKeyAction.run(
      async () => {
        await deleteApiKey({ id: keyId })
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(keyId)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        pendingDeleteKey.value = null
        await options.reload()
        toast.success('删除成功')
      },
      { onError: () => void options.reload() },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value || options.selectedIds.value.size === 0)
      return

    await batchDeletingAction.run(
      async () => {
        const deleteCount = options.selectedIds.value.size
        for (const keyId of [...options.selectedIds.value]) {
          await deleteApiKey({ id: keyId })
          const remaining = new Set(options.selectedIds.value)
          remaining.delete(keyId)
          options.selectedIds.value = remaining
        }
        showDeleteModal.value = false
        await options.reload()
        toast.success(`已删除 ${deleteCount} 个 API Key`)
      },
      { onError: () => void options.reload() },
    )
  }

  async function handleToggleStatus(key: ApiKeyRow) {
    await updatingStatusKeys.run(key.id, async () => {
      try {
        const mutation = key.enabled ? disableApiKey : enableApiKey
        await mutation({ id: key.id })
        await options.reload()
        toast.success(key.enabled ? '已禁用' : '已启用')
      }
      catch {
        void options.reload()
      }
    })
  }

  async function copyToClipboard(text: string) {
    await copyText(text, { successText: '已复制到剪贴板', emptyErrorText: '复制失败' })
  }

  async function revealPlaintextKey(apiKey: ApiKeyRow) {
    try {
      const result = await revealingKeys.run(apiKey.id, () => revealApiKey({ id: apiKey.id }))
      if (!result)
        return undefined
      if (!result.plaintextKey) {
        toast.error('完整 API Key 不可用')
        return undefined
      }
      return result.plaintextKey
    }
    catch {
      return undefined
    }
  }

  async function copyApiKey(apiKey: ApiKeyRow) {
    const key = await revealPlaintextKey(apiKey)
    if (key)
      await copyToClipboard(key)
  }

  watch(showKeyModal, (open) => {
    if (!open) {
      createdKey.value = ''
      createdKeyName.value = ''
    }
  })
  watch(showFormModal, (open) => {
    if (!open && !savingKey.value) {
      editingKey.value = null
      form.value = emptyForm()
    }
  })

  return {
    showFormModal,
    showDeleteModal,
    showSingleDeleteModal,
    showKeyModal,
    showAllAccountsConfirm,
    createdKey,
    createdKeyName,
    createdKeyExcelEnabled,
    editingKey,
    pendingDeleteKey,
    savingKey,
    deletingKey,
    batchDeleting,
    updatingStatusKeyIds,
    revealingKeyIds,
    form,
    openCreate,
    openEdit,
    requestSave,
    confirmAllAccountsScope,
    requestDeleteKey,
    handleDelete,
    handleBatchDelete,
    handleToggleStatus,
    copyToClipboard,
    revealPlaintextKey,
    copyApiKey,
  }
}

function emptyForm(): ApiKeyFormValue {
  return {
    openaiClientProfileOverride: null,
    xaiClientProfileOverride: null,
    scopeMode: 'account',
    accountIds: [],
    rotationStrategy: '',
    accountId: '',
    excelBridgeEnabled: false,
    customKey: '',
    name: '',
    label: '',
    groupIds: [],
    maxConcurrency: '',
    requestsPerMinute: '',
    dailyLimitUsd: '',
    weeklyLimitUsd: '',
  }
}

function limitInputValue(limit: string | number) {
  return Number(limit) === 0 ? '' : String(limit)
}

function parseLimit(value: string) {
  return Number(value)
}

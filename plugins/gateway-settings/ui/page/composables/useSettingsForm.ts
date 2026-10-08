import type { rotationOptions } from '@kit/rotation'
import type { RequestLocation } from '@/api'
import type { ClientProfileSelection, XaiClientProfileSelection } from '@/api/modules/client-profiles'
import { usePluginPolling } from '@sdk/polling'
import { computed, reactive, ref, shallowRef, watch } from 'vue'

import { getSettings, updateSettings } from '@/api'
import { updateRequestLocation } from '@/api/modules/settings'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/async'
import { normalizeRequestLocation, requestLocationError } from '@/utils/request-location'

type RotationStrategy = (typeof rotationOptions)[number]['value']

const MIB = 1024 * 1024

export function useSettingsForm() {
  const savingLocation = shallowRef(false)
  const locationStatus = shallowRef('位置更改会自动保存到服务器')
  const loading = shallowRef(true)
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const error = shallowRef('')
  const mappings = ref<Array<{ requestedModel: string, upstreamModel: string }>>([])
  const savedRequestLocation = shallowRef<RequestLocation>()
  const form = reactive({
    openaiClientProfile: null as ClientProfileSelection | null,
    xaiClientProfile: null as XaiClientProfileSelection | null,
    requestLocationEnabled: false,
    requestLocation: { country: '', region: '', city: '', timezone: '' },
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    requestIntervalMs: null as number | null,
    maxWaitingPerKey: null as number | null,
    maxWaitingPerAccount: null as number | null,
    concurrencyWaitTimeoutSeconds: null as number | null,
    gatewayMaxRequests: null as number | null,
    gatewayMaxBodyMiB: null as number | null,
    gatewayBodyBudgetMiB: null as number | null,

    rotationStrategy: '' as RotationStrategy | '',
    minCodexDesktopVersion: '',
    minCodexCliVersion: '',
    usageRetentionDays: 31,
    opsEventRetentionDays: 30,
    auditRetentionDays: 90,

    accountAutoFreezeEnabled: false,
    accountAutoFreezeThreshold: null as number | null,
    accountAutoFreezeWindowSeconds: null as number | null,
    accountAutoFreezeDurationSeconds: null as number | null,
    accountAutoFreezeProbeEnabled: true,
    accountAutoFreezeProbeModel: '',
    accountAutoFreezeAdaptiveConcurrency: true,
  })

  function snapshot() {
    return {
      form: { ...form, requestLocation: { ...form.requestLocation } },
      mappings: mappings.value.map(row => ({ ...row })),
    }
  }

  const saved = shallowRef<ReturnType<typeof snapshot>>()
  const loaded = computed(() => saved.value !== undefined)
  const hasChanges = computed(() => loaded.value && JSON.stringify(snapshot()) !== JSON.stringify(saved.value))

  async function saveLocation() {
    if (!saved.value || savingLocation.value || loading.value)
      return
    savingLocation.value = true
    try {
      // 串行保存并合并连续选择，只更新位置字段，不覆盖其他管理员的并发设置。
      while (saved.value && (JSON.stringify(form.requestLocation) !== JSON.stringify(saved.value.form.requestLocation) || form.requestLocationEnabled !== saved.value.form.requestLocationEnabled)) {
        const enabled = form.requestLocationEnabled
        const location = normalizeRequestLocation(form.requestLocation)
        const invalid = requestLocationError(location)
        if (invalid) {
          locationStatus.value = invalid
          return
        }
        locationStatus.value = '正在保存位置…'
        await updateRequestLocation(enabled, location)
        savedRequestLocation.value = { ...location }
        saved.value = { ...saved.value, form: { ...saved.value.form, requestLocationEnabled: enabled, requestLocation: { ...location } } }
        locationStatus.value = '位置已保存到服务器，其他设备共享'
      }
    }
    catch { locationStatus.value = '位置保存失败，点击重试' }
    finally { savingLocation.value = false }
  }
  watch(() => [form.requestLocationEnabled, JSON.stringify(form.requestLocation)], () => {
    void saveLocation()
  })

  function resetSettings() {
    if (!saved.value || saving.value || savingLocation.value)
      return
    Object.assign(form, saved.value.form, { requestLocation: { ...saved.value.form.requestLocation } })
    mappings.value = saved.value.mappings.map(row => ({ ...row }))
  }

  function numericModel(key: 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'requestIntervalMs' | 'maxWaitingPerKey' | 'maxWaitingPerAccount' | 'concurrencyWaitTimeoutSeconds' | 'gatewayMaxRequests' | 'gatewayMaxBodyMiB' | 'gatewayBodyBudgetMiB' | 'accountAutoFreezeThreshold' | 'accountAutoFreezeWindowSeconds' | 'accountAutoFreezeDurationSeconds') {
    return computed({
      get: () => (form[key] === null ? '' : String(form[key])),
      set: (value: string) => {
        if (!value.trim()) {
          form[key] = null
          return
        }
        const parsed = Number(value)
        form[key] = Number.isFinite(parsed) ? parsed : null
      },
    })
  }

  const refreshMarginSecondsValue = numericModel('refreshMarginSeconds')
  const refreshConcurrencyValue = numericModel('refreshConcurrency')
  const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
  const requestIntervalMsValue = numericModel('requestIntervalMs')
  const maxWaitingPerKeyValue = numericModel('maxWaitingPerKey')
  const maxWaitingPerAccountValue = numericModel('maxWaitingPerAccount')
  const gatewayMaxRequestsValue = numericModel('gatewayMaxRequests')
  const gatewayMaxBodyMiBValue = numericModel('gatewayMaxBodyMiB')
  const gatewayBodyBudgetMiBValue = numericModel('gatewayBodyBudgetMiB')
  const concurrencyWaitTimeoutSecondsValue = numericModel('concurrencyWaitTimeoutSeconds')
  const accountAutoFreezeThresholdValue = numericModel('accountAutoFreezeThreshold')
  const accountAutoFreezeWindowSecondsValue = numericModel('accountAutoFreezeWindowSeconds')
  const accountAutoFreezeDurationSecondsValue = numericModel('accountAutoFreezeDurationSeconds')

  const minCodexDesktopVersionError = computed(() => versionError(form.minCodexDesktopVersion))
  const minCodexCliVersionError = computed(() => versionError(form.minCodexCliVersion))

  function versionError(value: string): string {
    const normalized = value.trim()
    return normalized && !isSemver(normalized) ? '请输入标准 SemVer，例如 0.159.2' : ''
  }

  function applySettings(data: Awaited<ReturnType<typeof getSettings>>) {
    savedRequestLocation.value = { ...data.requestLocation }
    form.requestLocationEnabled = data.requestLocationEnabled
    form.requestLocation = { ...data.requestLocation }
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.requestIntervalMs = data.requestIntervalMs
    form.maxWaitingPerKey = data.maxWaitingPerKey
    form.maxWaitingPerAccount = data.maxWaitingPerAccount
    form.concurrencyWaitTimeoutSeconds = data.concurrencyWaitTimeoutSeconds
    form.gatewayMaxRequests = data.inferenceLimits.maxRequests
    form.gatewayMaxBodyMiB = (data.inferenceLimits.maxBodyBytes || data.responsesMaxDecompressedBodyBytes) / MIB
    form.gatewayBodyBudgetMiB = data.inferenceLimits.maxInFlightBodyBytes / MIB

    form.rotationStrategy = data.rotationStrategy
    form.minCodexDesktopVersion = data.minCodexDesktopVersion ?? ''
    form.openaiClientProfile = data.openaiClientProfile
    form.xaiClientProfile = data.xaiClientProfile
    form.minCodexCliVersion = data.minCodexCliVersion ?? ''
    form.usageRetentionDays = data.usageRetentionDays
    form.opsEventRetentionDays = data.opsEventRetentionDays
    form.auditRetentionDays = data.auditRetentionDays
    form.accountAutoFreezeEnabled = data.accountAutoFreezeEnabled
    form.accountAutoFreezeThreshold = data.accountAutoFreezeThreshold
    form.accountAutoFreezeWindowSeconds = data.accountAutoFreezeWindowSeconds
    form.accountAutoFreezeDurationSeconds = data.accountAutoFreezeDurationSeconds
    form.accountAutoFreezeProbeEnabled = data.accountAutoFreezeProbeEnabled
    form.accountAutoFreezeProbeModel = data.accountAutoFreezeProbeModel ?? ''
    form.accountAutoFreezeAdaptiveConcurrency = data.accountAutoFreezeAdaptiveConcurrency
    mappings.value = Object.entries(data.modelMappings || {}).map(([requestedModel, upstreamModel]) => ({
      requestedModel,
      upstreamModel: String(upstreamModel),
    }))
    saved.value = snapshot()
  }

  async function loadSettings(silent = false) {
    loading.value = true
    error.value = ''
    try {
      applySettings(await getSettings({ silent }))
    }
    catch (cause: unknown) {
      error.value = errorMessage(cause)
    }
    finally {
      loading.value = false
    }
  }

  usePluginPolling(async () => {
    if (!loaded.value || loading.value || saving.value || savingLocation.value || hasChanges.value)
      return
    const baseline = saved.value
    const data = await getSettings({ silent: true })
    // 请求期间开始编辑或保存时，不能用旧读取结果覆盖新表单。
    if (saved.value === baseline && !hasChanges.value && !saving.value && !savingLocation.value && !loading.value)
      applySettings(data)
  }, shallowRef(30_000))

  function addMapping() {
    mappings.value = [...mappings.value, { requestedModel: '', upstreamModel: '' }]
  }

  function updateMapping(index: number, key: 'requestedModel' | 'upstreamModel', value: string) {
    const rows = [...mappings.value]
    if (!rows[index])
      return
    rows[index] = { ...rows[index], [key]: value }
    mappings.value = rows
  }

  function removeMapping(index: number) {
    const rows = [...mappings.value]
    rows.splice(index, 1)
    mappings.value = rows
  }

  function mappingPayload() {
    const entries: Record<string, string> = {}
    for (const row of mappings.value) {
      const requested = row.requestedModel.trim()
      const upstream = row.upstreamModel.trim()
      if (!requested || !upstream)
        throw new Error('请完整填写模型映射')
      if (entries[requested])
        throw new Error(`存在重复的客户端模型：${requested}`)
      entries[requested] = upstream
    }
    return entries
  }

  async function saveSettings() {
    if (saving.value || savingLocation.value || loading.value || !savedRequestLocation.value || !form.openaiClientProfile || !form.xaiClientProfile)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, requestIntervalMs, rotationStrategy, maxWaitingPerKey, maxWaitingPerAccount, concurrencyWaitTimeoutSeconds, accountAutoFreezeThreshold, accountAutoFreezeWindowSeconds, accountAutoFreezeDurationSeconds } = form
    if (refreshMarginSeconds === null || refreshConcurrency === null || maxConcurrentPerAccount === null || requestIntervalMs === null || !rotationStrategy || maxWaitingPerKey === null || maxWaitingPerAccount === null || concurrencyWaitTimeoutSeconds === null) {
      toast.warning('请完整填写并发、队列、凭据刷新参数和调度策略')
      return
    }
    if (!Number.isInteger(maxConcurrentPerAccount) || maxConcurrentPerAccount < 0 || maxConcurrentPerAccount > 4294967295) {
      toast.warning('默认账号并发上限应为 0～4294967295 的整数，0 表示不限制')
      return
    }
    if (![maxWaitingPerKey, maxWaitingPerAccount].every(value => Number.isInteger(value) && value >= 0 && value <= 1000)
      || !Number.isInteger(concurrencyWaitTimeoutSeconds) || concurrencyWaitTimeoutSeconds < 1 || concurrencyWaitTimeoutSeconds > 120) {
      toast.warning('队列容量应为 0～1000 的整数，排队超时应为 1～120 秒的整数')
      return
    }
    if (minCodexDesktopVersionError.value || minCodexCliVersionError.value) {
      toast.warning('请修正客户端最低版本格式')
      return
    }
    const requestLocation = normalizeRequestLocation(form.requestLocation)
    const locationError = requestLocationError(requestLocation)
    if (locationError) {
      toast.warning(locationError)
      return
    }
    if (accountAutoFreezeThreshold === null || accountAutoFreezeWindowSeconds === null || accountAutoFreezeDurationSeconds === null) {
      toast.warning('请完整填写过载保护参数')
      return
    }
    if (!Number.isInteger(accountAutoFreezeThreshold) || accountAutoFreezeThreshold < 2 || accountAutoFreezeThreshold > 1000
      || !Number.isInteger(accountAutoFreezeWindowSeconds) || accountAutoFreezeWindowSeconds < 60 || accountAutoFreezeWindowSeconds > 3600
      || !Number.isInteger(accountAutoFreezeDurationSeconds) || accountAutoFreezeDurationSeconds < 300 || accountAutoFreezeDurationSeconds > 604800) {
      toast.warning('失败次数阈值应为 2～1000，统计窗口为 60～3600 秒，冷却时长为 300～604800 秒')
      return
    }
    const probeModel = form.accountAutoFreezeProbeModel.trim()
    if (probeModel && (probeModel.length > 128 || probeModel !== probeModel.trim())) {
      toast.warning('探测模型名称不能超过 128 个字符')
      return
    }
    const { gatewayMaxRequests, gatewayMaxBodyMiB, gatewayBodyBudgetMiB } = form
    if (gatewayMaxRequests === null || !Number.isInteger(gatewayMaxRequests) || gatewayMaxRequests < 0 || gatewayMaxRequests > 65535
      || gatewayMaxBodyMiB === null || !Number.isInteger(gatewayMaxBodyMiB) || gatewayMaxBodyMiB < 1 || gatewayMaxBodyMiB > 4095
      || gatewayBodyBudgetMiB === null || !Number.isInteger(gatewayBodyBudgetMiB) || gatewayBodyBudgetMiB < 0 || gatewayBodyBudgetMiB > 4095
      || (gatewayBodyBudgetMiB > 0 && gatewayBodyBudgetMiB < gatewayMaxBodyMiB)) {
      toast.warning('并发应为 0～65535；正文应为 1～4095 MiB，总预算应不小于单请求上限（0 表示不限）')
      return
    }
    const xaiClientProfile = form.xaiClientProfile
    const openaiClientProfile = form.openaiClientProfile
    await saveAction.run(async () => {
      const result = await updateSettings({
        openaiClientProfile,
        xaiClientProfile,
        requestLocationEnabled: form.requestLocationEnabled,
        requestLocation,
        modelMappings: mappingPayload(),
        refreshMarginSeconds,
        refreshConcurrency,
        maxConcurrentPerAccount,
        requestIntervalMs,
        maxWaitingPerKey,
        maxWaitingPerAccount,
        concurrencyWaitTimeoutSeconds,
        inferenceLimits: { maxRequests: gatewayMaxRequests, maxBodyBytes: gatewayMaxBodyMiB * MIB, maxInFlightBodyBytes: gatewayBodyBudgetMiB * MIB },
        responsesMaxDecompressedBodyBytes: gatewayMaxBodyMiB * MIB,
        rotationStrategy,
        minCodexDesktopVersion: form.minCodexDesktopVersion.trim() || null,
        minCodexCliVersion: form.minCodexCliVersion.trim() || null,
        usageRetentionDays: form.usageRetentionDays,
        opsEventRetentionDays: form.opsEventRetentionDays,
        auditRetentionDays: form.auditRetentionDays,
        accountAutoFreezeEnabled: form.accountAutoFreezeEnabled,
        accountAutoFreezeThreshold,
        accountAutoFreezeWindowSeconds,
        accountAutoFreezeDurationSeconds,
        accountAutoFreezeProbeEnabled: form.accountAutoFreezeProbeEnabled,
        accountAutoFreezeProbeModel: probeModel || null,
        accountAutoFreezeAdaptiveConcurrency: form.accountAutoFreezeAdaptiveConcurrency,
      })
      applySettings(result)
      toast.success('设置已保存')
    }, {
      onError: (cause) => {
        if (cause instanceof ApiError)
          void loadSettings(true)
      },
    })
  }

  return {
    savingLocation,
    locationStatus,
    saveLocation,
    loading,
    saving,
    hasChanges,
    resetSettings,
    error,
    form,
    mappings,
    addMapping,
    updateMapping,
    removeMapping,
    refreshMarginSecondsValue,
    refreshConcurrencyValue,
    maxConcurrentPerAccountValue,
    requestIntervalMsValue,
    maxWaitingPerKeyValue,
    maxWaitingPerAccountValue,
    concurrencyWaitTimeoutSecondsValue,
    gatewayMaxRequestsValue,
    gatewayMaxBodyMiBValue,
    gatewayBodyBudgetMiBValue,
    accountAutoFreezeThresholdValue,
    accountAutoFreezeWindowSecondsValue,
    accountAutoFreezeDurationSecondsValue,
    minCodexDesktopVersionError,
    minCodexCliVersionError,
    saveSettings,
    loadSettings,
  }
}

function isSemver(value: string): boolean {
  if (value.length > 64 || value.startsWith('v'))
    return false

  const buildParts = value.split('+')
  if (buildParts.length > 2)
    return false
  const [versionAndPrerelease = '', build] = buildParts
  if (build !== undefined && !validIdentifiers(build, false))
    return false

  const prereleaseSeparator = versionAndPrerelease.indexOf('-')
  const core = prereleaseSeparator < 0
    ? versionAndPrerelease
    : versionAndPrerelease.slice(0, prereleaseSeparator)
  const prerelease = prereleaseSeparator < 0
    ? undefined
    : versionAndPrerelease.slice(prereleaseSeparator + 1)
  if (prerelease !== undefined && !validIdentifiers(prerelease, true))
    return false

  const coreParts = core.split('.')
  return coreParts.length === 3 && coreParts.every(validCoreNumericIdentifier)
}

function validIdentifiers(value: string, rejectNumericLeadingZeros: boolean): boolean {
  return Boolean(value) && value.split('.').every((identifier) => {
    if (!identifier || !/^[\da-z-]+$/i.test(identifier))
      return false
    return !rejectNumericLeadingZeros || !/^\d+$/.test(identifier) || validNumericIdentifier(identifier)
  })
}

function validNumericIdentifier(value: string): boolean {
  return /^(?:0|[1-9]\d*)$/.test(value)
}

function validCoreNumericIdentifier(value: string): boolean {
  return validNumericIdentifier(value) && BigInt(value) <= 18_446_744_073_709_551_615n
}

import type { WarmupAccount, WarmupOverride, WarmupSettings, WarmupTask } from '@/api/modules/accountWarmup'

export function defaultWarmupSettings(): WarmupSettings {
  return { enabled: false, timezone: 'Asia/Shanghai', dailyEnabled: false, dailyTime: '08:00', resetEnabled: true, resetDelaySeconds: 30, jitterMinSeconds: 5, jitterMaxSeconds: 15, targetMode: 'all', accountIds: [], defaultModel: 'gpt-6.1-sol', prompt: 'hi', reasoningEffort: 'auto', accountOverrides: [] }
}
export function cloneSettings(value: WarmupSettings): WarmupSettings {
  const cloned = JSON.parse(JSON.stringify(value)) as WarmupSettings
  // 旧服务返回的配置没有请求字段；补齐默认值后再建立草稿基线。
  return {
    ...cloned,
    prompt: cloned.prompt ?? 'hi',
    reasoningEffort: cloned.reasoningEffort ?? 'auto',
    accountOverrides: cloned.accountOverrides.map(item => ({ ...item, reasoningEffort: item.reasoningEffort ?? null })),
  }
}
export function settingsError(value: WarmupSettings) {
  const time = (input: string) => /^(?:[01]\d|2[0-3]):[0-5]\d$/.test(input)
  const seconds = (input: number) => Number.isSafeInteger(input) && input >= 0 && input <= 86400
  if (!value.timezone.trim())
    return '请输入调度时区'
  try {
    new Intl.DateTimeFormat('zh-CN', { timeZone: value.timezone }).format()
  }
  catch { return '请输入有效的 IANA 时区，例如 Asia/Shanghai' }
  if (!time(value.dailyTime))
    return '每日执行时间格式为 HH:mm'
  if (value.enabled && !value.dailyEnabled && !value.resetEnabled)
    return '启用自动预热时，请至少选择一种触发方式'
  if (!value.defaultModel.trim())
    return '请输入默认预热模型'
  if (!seconds(value.resetDelaySeconds))
    return '重置延迟须为 0 至 86400 秒的整数'
  if (![value.jitterMinSeconds, value.jitterMaxSeconds].every(item => Number.isSafeInteger(item) && item >= 0 && item <= 300) || value.jitterMinSeconds > value.jitterMaxSeconds)
    return '随机延迟须为 0 至 300 秒，且最小值不大于最大值'
  if (value.targetMode === 'selected' && (value.accountIds.length === 0 || value.accountIds.length > 500))
    return '指定账号须选择 1 至 500 个账号'
  for (const item of value.accountOverrides) {
    if (item.dailyTime !== null && !time(item.dailyTime))
      return '账号独立时间格式为 HH:mm'
    if (item.model !== null && !item.model.trim())
      return '独立模型不能为空，留空请选择跟随默认'
    if (item.resetDelaySeconds !== null && !seconds(item.resetDelaySeconds))
      return '账号独立重置延迟须为 0 至 86400 秒的整数'
  }
  return ''
}
export function accountOverride(value: WarmupSettings, accountId: string): WarmupOverride {
  const override = value.accountOverrides.find(item => item.accountId === accountId)
  return override ? { ...override, reasoningEffort: override.reasoningEffort ?? null } : { accountId, enabled: true, dailyTime: null, model: null, resetDelaySeconds: null, reasoningEffort: null }
}
export const reasonLabels: Record<string, string> = {
  unsupported: '暂不支持此账号类型',
  unsupported_account: '暂不支持此账号类型',
  unsupported_provider: '暂不支持此服务商',
  unsupported_authentication: '仅支持 OpenAI OAuth',
  disabled: '账号已停用',
  account_disabled: '账号已停用',
  archived: '账号已归档',
  account_archived: '账号已归档',
  quota_exhausted: '官方额度不足',
  quota_protection: '已达到养号阈值',
  quota_protection_reached: '已达到养号阈值',
  model_not_allowed: '账号政策不允许此模型',
  model_denied: '账号政策不允许此模型',
  reasoning_unsupported: '模型目录未确认支持所选推理强度',
  rate_limited: '账号冷却中',
  credential_invalid: '账号需要重新授权',
  credential_expired: '凭据已过期',
  account_busy: '账号正在使用',
  cancelled: '已取消未发送项',
  duplicate: '已有活动任务',
  unknown: '结果未知，不会自动重发',
}
export const reasonText = (code: string | null | undefined) => code ? reasonLabels[code] ?? code : ''
export function accountAvailability(account: WarmupAccount) {
  if (!account.supported)
    return (account.reasonCode && reasonLabels[account.reasonCode]) || '仅支持 OpenAI OAuth，当前类型暂不支持'
  if (!account.enabled)
    return '账号已停用'
  return reasonText(account.reasonCode) || (account.modelAllowed === false ? '账号政策不允许此模型' : '可预热')
}
export const taskLabels: Record<WarmupTask['status'], string> = { queued: '等待执行', running: '执行中', accepted: '上游已收头', failed: '失败', skipped: '已跳过', unknown: '结果未知', cancelled: '已取消' }
export const sourceLabels = { manual: '手动', daily: '每日计划', reset: '额度重置' }
export function formatWarmupTime(value: string | null | undefined) {
  if (!value)
    return '—'
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? '—' : date.toLocaleString('zh-CN', { hour12: false })
}

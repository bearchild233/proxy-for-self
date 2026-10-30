import type { AccountStatus } from './accounts'
import { call } from '@sdk/ui'

export type WarmupReasoningEffort = 'auto' | 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh'
export interface WarmupModel {
  id: string
  label: string
  reasoningEfforts?: string[]
}
export interface WarmupRequest {
  model: string
  prompt: string
  reasoningEffort: WarmupReasoningEffort
}
const effortLabels: Record<WarmupReasoningEffort, string> = {
  auto: '自动（最低受支持强度）',
  none: '无 · None',
  minimal: '最小 · Minimal',
  low: '低 · Low',
  medium: '中 · Medium',
  high: '高 · High',
  xhigh: '极高 · XHigh',
}
export function warmupEffortOptions(model?: WarmupModel, selected: WarmupReasoningEffort = 'auto') {
  return (Object.keys(effortLabels) as WarmupReasoningEffort[])
    .filter(value => value === 'auto' || !model?.reasoningEfforts || model.reasoningEfforts.includes(value) || value === selected)
    .map(value => ({
      value,
      label: effortLabels[value] + (value !== 'auto' && model?.reasoningEfforts && !model.reasoningEfforts.includes(value) ? '（该模型不支持）' : ''),
      disabled: value !== 'auto' && model?.reasoningEfforts !== undefined && !model.reasoningEfforts.includes(value),
    }))
}
export function warmupRequestError(value: { model?: string, prompt: string, reasoningEffort: string }) {
  if (value.model !== undefined && (!value.model.trim() || new TextEncoder().encode(value.model).length > 128 || [...value.model].some((char) => {
    const code = char.codePointAt(0) ?? 0
    return code < 32 || (code >= 127 && code <= 159)
  }))) {
    return '请输入有效的预热模型 ID（最多 128 字节）'
  }
  if (!value.prompt.trim())
    return '请输入预热提示词'
  if (new TextEncoder().encode(value.prompt).length > 4096)
    return '预热提示词不能超过 4096 字节'
  if ([...value.prompt].some((char) => {
    const code = char.codePointAt(0) ?? 0
    return (code < 32 || (code >= 127 && code <= 159)) && !['\n', '\r', '\t'].includes(char)
  })) {
    return '预热提示词包含无效控制字符'
  }
  if (!Object.hasOwn(effortLabels, value.reasoningEffort))
    return '请选择有效的推理强度'
  return ''
}

export interface WarmupOverride {
  accountId: string
  enabled: boolean
  dailyTime: string | null
  model: string | null
  reasoningEffort: WarmupReasoningEffort | null
  resetDelaySeconds: number | null
}
export interface WarmupSettings {
  enabled: boolean
  timezone: string
  dailyEnabled: boolean
  dailyTime: string
  resetEnabled: boolean
  resetDelaySeconds: number
  jitterMinSeconds: number
  jitterMaxSeconds: number
  targetMode: 'all' | 'selected'
  accountIds: string[]
  defaultModel: string
  prompt: string
  reasoningEffort: WarmupReasoningEffort
  accountOverrides: WarmupOverride[]
}
export interface WarmupAccount {
  id: string
  provider: string
  authenticationKind: string
  enabled: boolean
  status: AccountStatus
  supported: boolean
  reasonCode: string | null
  modelAllowed: boolean | null
  models: WarmupModel[] | null
  shortTerm: { observedAt: string | null, windowSeconds: number | null, usedPercent: number | null, resetAt: string | null } | null
}
export interface WarmupRun {
  id: string
  source: 'manual' | 'daily' | 'reset'
  status: 'queued' | 'running' | 'completed' | 'cancelled'
  createdAt: string
  finishedAt: string | null
  total: number
  accepted: number
  failed: number
  skipped: number
  unknown: number
  pending: number
}
export interface WarmupTask {
  id: string
  accountId: string
  source: WarmupRun['source']
  model: string
  prompt: string
  reasoningEffort: WarmupReasoningEffort
  status: 'queued' | 'running' | 'accepted' | 'failed' | 'skipped' | 'unknown' | 'cancelled'
  scheduledAt: string | null
  dueAt: string | null
  startedAt: string | null
  finishedAt: string | null
  durationMs: number | null
  reasonCode: string | null
  httpStatus: number | null
  resetAt: string | null
  timerConfirmed: boolean
}
export interface WarmupAccountState {
  status: 'pending' | 'queued' | 'running' | 'warmed' | 'unconfirmed' | 'unknown' | 'failed' | 'skipped' | 'unsupported'
  taskId: string | null
  updatedAt: string | null
  resetAt: string | null
  reasonCode: string | null
}
export interface WarmupAvailable {
  available: true
  revision: number
  settings: WarmupSettings
  accounts: WarmupAccount[]
  accountStates?: Record<string, WarmupAccountState>
  runtime: { ready: boolean, nextRunAt: string | null, nextAutomaticRunAt?: string | null, lastRun: WarmupRun | null }
}
export type WarmupRead = WarmupAvailable | { available: false, reason: 'plugin_disabled' | 'service_unavailable' }
export interface WarmupRunInput {
  submissionId: string
  scope: 'all' | 'selected'
  accountIds?: string[]
  model?: string
  prompt?: string
  reasoningEffort?: WarmupReasoningEffort
}
export const readAccountWarmup = (input: { accountIds?: string[], includeModels?: boolean, model?: string } = {}) => call<WarmupRead>('account-warmup.read', input)
export const configureAccountWarmup = (expectedRevision: number, settings: WarmupSettings) => call<WarmupRead>('account-warmup.configure', { expectedRevision, settings })
export const runAccountWarmup = (input: WarmupRunInput) => call<{ run: WarmupRun }>('account-warmup.run', input)
export const accountWarmupHistory = (input: { limit?: number, runId?: string, source?: 'automatic' | 'manual' } = {}) => call<{ runs: WarmupRun[], items: WarmupTask[] }>('account-warmup.history', input)
export const cancelAccountWarmup = (runId: string) => call<{ run: WarmupRun }>('account-warmup.cancel', { runId })

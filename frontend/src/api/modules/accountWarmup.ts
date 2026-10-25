import type { AccountStatus } from './accounts'
import { call } from '@sdk/ui'

export interface WarmupOverride {
  accountId: string
  enabled: boolean
  dailyTime: string | null
  model: string | null
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
  models: Array<{ id: string, label: string }> | null
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
export interface WarmupAvailable {
  available: true
  revision: number
  settings: WarmupSettings
  accounts: WarmupAccount[]
  runtime: { ready: boolean, nextRunAt: string | null, lastRun: WarmupRun | null }
}
export type WarmupRead = WarmupAvailable | { available: false, reason: 'plugin_disabled' | 'service_unavailable' }
export interface WarmupRunInput {
  submissionId: string
  scope: 'all' | 'selected'
  accountIds?: string[]
}
export const readAccountWarmup = (input: { accountIds?: string[], includeModels?: boolean, model?: string } = {}) => call<WarmupRead>('account-warmup.read', input)
export const configureAccountWarmup = (expectedRevision: number, settings: WarmupSettings) => call<WarmupRead>('account-warmup.configure', { expectedRevision, settings })
export const runAccountWarmup = (input: WarmupRunInput) => call<{ run: WarmupRun }>('account-warmup.run', input)
export const accountWarmupHistory = (input: { limit?: number, runId?: string } = {}) => call<{ runs: WarmupRun[], items: WarmupTask[] }>('account-warmup.history', input)
export const cancelAccountWarmup = (runId: string) => call<{ run: WarmupRun }>('account-warmup.cancel', { runId })

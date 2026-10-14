import type { Observation } from './model'
import { formatDateTime } from '@/utils/date'

export type TimingMetric = 'preparationSeconds' | 'windowSeconds' | 'businessSeconds'
export const timingLabels: Record<TimingMetric, string> = {
  preparationSeconds: '找票耗时',
  windowSeconds: '窗口时长',
  businessSeconds: '业务使用',
}
export interface TimingSummary {
  preparationSeconds: number
  windowSeconds: number
  businessSeconds: number
  missing: Record<TimingMetric, number>
  requests: number
  records: number
  incomplete: number
}
export interface UsageSegment {
  id: string
  startedAt: number
  endedAt: number | null
  closedAt: number | null
  reason: string | null
  businessSeconds: number
  requestSeconds: number
  requests: number
  incomplete: boolean
}
export interface UsageRecord {
  id: string
  accountId: string
  gateway: string
  batchId: string
  batchNumber: number
  preparationStartedAt: number
  probeEndedAt: number | null
  probeResult: '21' | '29' | 'failed' | null
  observations: Observation[]
  observationCount: number
  firstContactAt: number | null
  verifiedAt: number | null
  retiredAt: number | null
  drainedAt: number | null
  /** 备用票：拿到时间、启用时间；窗口时长按最后一次确认可用计。 */
  standby?: boolean
  standbyAt?: number | null
  activatedAt?: number | null
  windowStartAt?: number | null
  windowEndAt?: number | null
  lastConfirmedAt?: number | null
  preparationSeconds: number | null
  windowSeconds: number | null
  businessSeconds: number | null
  knownBusinessSeconds: number
  requestSeconds: number
  ticketSeconds: number
  requests: number
  state: string
  reason: string | null
  incomplete: boolean
  partial: boolean
  segments: UsageSegment[]
}
export interface UsageBatch extends TimingSummary {
  id: string
  number: number
  accountId: string
  startedAt: number
  endedAt: number | null
  candidates: string[]
  processed: Record<string, string>
  served: number
  status: 'active' | 'complete' | 'interrupted'
  reason: string | null
  partial: boolean
}
export interface CoolingStep {
  generation: string | null
  startedAt: number | null
  scheduledStartAt: number
  plannedSeconds: number
  dueAt: number
  probeAt: number | null
  resultAt: number | null
  result: '21' | '29' | 'failed' | null
  status: number | null
  learnedRestSeconds: number | null
  /** 这一轮静置的原因、所在的档与当时的起始档；复测失败的类别在 failure。 */
  reason?: string | null
  tierSeconds?: number | null
  startTierSeconds?: number | null
  failure?: string | null
}
export interface CoolingCycle {
  id: number
  accountId: string
  gateway: string
  startedAt: number
  recoveredAt: number | null
  reason: string | null
  state: 'cooling' | 'due' | 'probing' | 'recovered'
  incomplete: boolean
  steps: CoolingStep[]
}
export interface GatewaySummary extends TimingSummary { gateway: string }
export interface CoolingCounts {
  recovered: number
  unrecovered: number
  failed: number
  pending: number
}
export interface CoolingAnalysis extends CoolingCounts {
  total: number
  gateways: number
  missingInterval: number
  recoveredMedianSeconds: number | null
  bins: (CoolingCounts & { label: string })[]
}
export interface RecentWindow {
  accountId: string
  gateway: string
  windowSeconds: number
  windows: number
}
export interface PoolStatistics {
  version: 2
  recordedSince: number
  collectionGaps: number
  from: number
  to: number
  page: { number: number, size: number, total: number, pages: number }
  summary: TimingSummary
  gateways: GatewaySummary[]
  records: UsageRecord[]
  batches: UsageBatch[]
  recoveries: CoolingCycle[]
  coolingAnalysis: CoolingAnalysis | null
  /** 最近 24 小时每个账号各网关的窗口时长（仅汇总视图）。 */
  recentWindows: RecentWindow[]
  options: {
    accounts: string[]
    gateways: string[]
    batches: { id: string, accountId: string, number: number }[]
  }
}
export const batchLabels = { active: '进行中', complete: '已完成', interrupted: '已中断' }
export const coolingLabels = { cooling: '静置中', due: '待复测', probing: '复测中', recovered: '已观测恢复' }
export const reasonLabels: Record<string, string> = {
  29: '29 退役',
  probe_failed: '检测失败',
  probe_interrupted: '检测中断',
  rotation: '窗口轮换',
  cookie_expiring: 'Cookie 到期',
  standby_expired: '备用票过期',
  round_end: '一轮结束',
  account_left: '账号退出调度',
  gateway_mismatch: '网关不一致',
  invalidated: '窗口失效',
  material_lost: '配套材料丢失',
  idle: '闲置满 2 小时',
  configuration: '配置变更',
  disabled: '插件停用',
}
// 检测失败的类别；HTTP 状态码另显示。
export const failureLabels: Record<string, string> = {
  http_status: '状态码异常',
  timeout: '超时',
  no_response: '无响应',
  pool_service: '池服务取 Cookie 失败',
  cookie_missing: '缺少 Cookie',
  cookie_host: 'Cookie 指向别的网关',
  state_missing: '缺少 state',
  account: '账号不可检测',
  stopped: '服务停止',
  internal: '内部错误',
}
export function recordStatus(record: UsageRecord): string {
  if (record.incomplete)
    return '记录不完整'
  if (record.retiredAt == null)
    return record.state === 'preparing' ? '找票中' : record.state === 'standby' ? '备用中' : '窗口可用'
  return reasonLabels[record.reason ?? ''] ?? '已退役'
}
export const historyTime = (value: number | null | undefined) => value == null ? '—' : formatDateTime(value * 1000)

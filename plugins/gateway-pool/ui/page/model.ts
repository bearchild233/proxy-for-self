// 网关池页面的数据结构与展示工具。所有时间戳均为秒。
import type { CoolingStep } from './history'

export interface CoolingVisit extends CoolingStep {
  idleSeconds: number | null
  afterDueSeconds: number
}

export interface CoolingSummary {
  current: CoolingStep | null
  visits: CoolingVisit[]
  hasMore: boolean
}
export interface Settings {
  mode: 'lazy' | 'continuous'
  windowSeconds: number
  initialRestSeconds: number
  maximumRestSeconds: number
  /** 复测仍降智时的升档方式：加性每次加一个步长，乘性每次翻倍。 */
  backoffMode: 'additive' | 'multiplicative'
  backoffStepSeconds: number
  probeIntervalMs: number
  probeModel: string
  poolDirect: boolean
  groupIds: string[]
  gateways: string[]
  poolAccountIds: Record<string, string>
  poolUrl: string
  poolTokenConfigured?: boolean
  /** 按降智检测结果自动在正常分组与降智分组之间调整账号。 */
  dynamicGroups?: boolean
  normalGroupId?: string
  degradedGroupId?: string
}

/** 一次自动分组调整的记录；status 为 pending 表示已决定、尚未回报结果。 */
export interface GroupEvent {
  id: string
  at: number
  accountId: string
  direction: 'degraded' | 'recovered'
  fromGroup: string
  toGroup: string
  model?: string | null
  status: 'pending' | 'moved' | 'unchanged' | 'failed' | 'unsupported'
  finishedAt?: number | null
}

export type PairState = 'unknown' | 'preparing' | 'standby' | 'ready' | 'draining' | 'cooling'

export interface Pair {
  coolingSummary?: CoolingSummary
  accountId: string
  gateway: string
  state: PairState
  generation?: string
  restUntil?: number | null
  restSeconds?: number
  /** 当前静置所在的档；检测失败的短静置不改档。 */
  tierSeconds?: number | null
  /** 本次静置开始时的起始档。 */
  startTierSeconds?: number | null
  learnedRestSeconds?: number | null
  inFlight: number
  firstContactAt?: number | null
  lastContactAt?: number | null
  retiredReason?: string
  /** 备用票：拿到时间与丢弃时间。 */
  standbyAt?: number | null
  standbyExpiresAt?: number | null
  /** 备用票转正后，窗口从第一次业务访问开始计时。 */
  fromStandby?: boolean
  activatedAt?: number | null
  windowStartedAt?: number | null
  /** 准备中的组合是备用票。 */
  standbyPlan?: boolean
  ticketModels?: string[]
  cookieExpiresAt?: number | null
}

export interface CatalogEntry {
  gateway: string
  validForS: number
  country?: string | null
}

export interface Observation {
  id: number
  at: number
  account: string
  gateway: string
  source: 'probe' | 'probe-attempt' | 'business' | 'ticket'
  model?: string | null
  phase?: 'bootstrap' | 'replay'
  retry?: number
  status?: number | null
  stateReturned?: boolean
  classification?: '21' | '29' | null
  durationMs?: number | null
  plannedRestSeconds?: number | null
  observedIdleSeconds?: number | null
}

export interface Switch {
  at: number
  accountId: string
  fromAccount?: string | null
  fromGateway?: string | null
  toGateway: string
  previousWindowSeconds?: number | null
  preparationSeconds: number
}

export interface AccountQueue {
  revision: number
  accounts: { accountId: string, enabled: boolean, quotaExhausted: boolean, reason: string | null }[]
}

export interface PoolView {
  accountQueue: AccountQueue
  preview?: boolean
  settings: Settings
  pairs: Pair[]
  observations: Observation[]
  switches: Switch[]
  catalogs?: Record<string, { at: number, gateways: CatalogEntry[] }>
  groupEvents?: GroupEvent[]
}

export interface DirectoryAccount {
  id: string
  name: string
  email?: string
  groups?: { id: string }[]
}

export const stateLabels: Record<PairState, string> = {
  ready: '服务中',
  standby: '备用票',
  preparing: '检测中',
  draining: '排空中',
  cooling: '静置中',
  unknown: '可检测',
}

export function duration(seconds?: number | null): string {
  if (seconds == null || !Number.isFinite(seconds))
    return '—'
  const value = Math.max(0, Math.round(seconds))
  if (value >= 3600) {
    const hours = Math.floor(value / 3600)
    const minutes = Math.floor((value % 3600) / 60)
    return minutes ? `${hours}h ${minutes}m` : `${hours}h`
  }
  if (value >= 60)
    return value % 60 ? `${Math.floor(value / 60)}m ${value % 60}s` : `${value / 60}m`
  return `${value}s`
}

// 窗口进度用计时器格式，例如 2:25。
export function clock(seconds: number): string {
  const value = Math.max(0, Math.floor(seconds))
  return `${Math.floor(value / 60)}:${String(value % 60).padStart(2, '0')}`
}

// 窗口计时起点：备用票转正的窗口从第一次业务访问算起，其余从首次接触算起。
export function windowStart(pair: Pair): number | null {
  return (pair.fromStandby ? pair.windowStartedAt : pair.firstContactAt) ?? null
}

export function gatewayNumber(gateway: string): string {
  return gateway.replace(/^unified-/, '')
}

export function sourceLabel(sample: Observation): string {
  if (sample.source === 'business')
    return '业务请求'
  if (sample.source === 'ticket')
    return '拿票'
  if (sample.source === 'probe')
    return '打票判定'
  return sample.phase === 'bootstrap' ? '取 S0' : sample.retry ? '重放 · 重试' : '重放'
}

export const groupEventLabels: Record<GroupEvent['status'], string> = {
  pending: '处理中',
  moved: '已移动',
  unchanged: '无需移动',
  failed: '移动失败',
  unsupported: '未启用写入',
}

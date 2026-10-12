// 网关池页面的数据结构与展示工具。所有时间戳均为秒。
export interface Settings {
  mode: 'lazy' | 'continuous'
  windowSeconds: number
  initialRestSeconds: number
  maximumRestSeconds: number
  probeIntervalMs: number
  probeModel: string
  poolDirect: boolean
  groupIds: string[]
  gateways: string[]
  poolAccountIds: Record<string, string>
  poolUrl: string
  poolTokenConfigured?: boolean
}

export type PairState = 'unknown' | 'preparing' | 'ready' | 'draining' | 'cooling'

export interface Pair {
  accountId: string
  gateway: string
  state: PairState
  generation?: string
  restUntil?: number | null
  restSeconds?: number
  learnedRestSeconds?: number | null
  inFlight: number
  firstContactAt?: number | null
  lastContactAt?: number | null
  retiredReason?: string
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

export interface PoolView {
  preview?: boolean
  settings: Settings
  pairs: Pair[]
  observations: Observation[]
  switches: Switch[]
  catalogs?: Record<string, { at: number, gateways: CatalogEntry[] }>
}

export interface DirectoryAccount {
  id: string
  name: string
  email?: string
  groups?: { id: string }[]
}

export const stateLabels: Record<PairState, string> = {
  ready: '服务中',
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

import type { OpsError } from '@/api'

const failureClassLabels: Readonly<Record<string, string>> = {
  continuation_recovery_required: '会话续接需要重建',
}

export function failureClassText(value: string | null | undefined) {
  if (!value)
    return '未记录'
  return failureClassLabels[value] ?? value
}

export function opsErrorSummary(record: OpsError) {
  const failureLabel = failureClassLabels[record.failureClass]
  return failureLabel ?? record.providerErrorCode ?? record.failureClass
}

export function errorOriginText(record: OpsError | null) {
  if (!record)
    return '未记录'
  if (record.upstreamStatusCode === 429)
    return '上游返回 429（限流 / 额度限制）'
  if (record.upstreamSendState === 'not_sent')
    return '网关阶段拒绝（本次未发送上游）'
  if (record.upstreamStatusCode !== null && record.upstreamStatusCode >= 400)
    return `上游返回 ${record.upstreamStatusCode}`
  return '证据不足，请结合错误详情判断'
}

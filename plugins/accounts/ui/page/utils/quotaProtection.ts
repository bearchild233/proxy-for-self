import type { AccountQuotaProtection } from '@/api'

export const QUOTA_PROTECTION_PRESETS = [85, 90, 95] as const

function validLimit(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 99
}

export function supportsQuotaProtection(value: unknown): value is AccountQuotaProtection {
  if (!value || typeof value !== 'object')
    return false
  const candidate = value as Partial<AccountQuotaProtection>
  return typeof candidate.enabled === 'boolean'
    && validLimit(candidate.usedPercentLimit)
}

export function supportsSplitQuotaProtection(value: unknown): value is AccountQuotaProtection & { shortTermUsedPercentLimit: number, weeklyUsedPercentLimit: number } {
  return supportsQuotaProtection(value)
    && validLimit(value.shortTermUsedPercentLimit) && validLimit(value.weeklyUsedPercentLimit)
}

export function parseQuotaProtectionLimit(input: string): number | null {
  const value = input.trim()
  if (!/^\d+$/.test(value))
    return null
  const limit = Number(value)
  return validLimit(limit) ? limit : null
}

export function parseQuotaProtection(enabled: boolean, input: string, shortTermInput?: string, weeklyInput?: string): AccountQuotaProtection | null {
  const usedPercentLimit = parseQuotaProtectionLimit(input)
  if (usedPercentLimit === null)
    return null
  if (shortTermInput === undefined && weeklyInput === undefined)
    return { enabled, usedPercentLimit }
  const shortTermUsedPercentLimit = parseQuotaProtectionLimit(shortTermInput ?? '')
  const weeklyUsedPercentLimit = parseQuotaProtectionLimit(weeklyInput ?? '')
  return shortTermUsedPercentLimit !== null && weeklyUsedPercentLimit !== null
    ? { enabled, usedPercentLimit, shortTermUsedPercentLimit, weeklyUsedPercentLimit }
    : null
}

export function quotaProtectionMatches(actual: unknown, expected: AccountQuotaProtection) {
  return supportsQuotaProtection(actual)
    && actual.enabled === expected.enabled && actual.usedPercentLimit === expected.usedPercentLimit
    && actual.shortTermUsedPercentLimit === expected.shortTermUsedPercentLimit
    && actual.weeklyUsedPercentLimit === expected.weeklyUsedPercentLimit
}

export function quotaProtectionThresholds(provider: string, protection: AccountQuotaProtection | undefined) {
  if (provider === 'openai') {
    const fallback = protection?.usedPercentLimit ?? 90
    return `5 小时 ${protection?.shortTermUsedPercentLimit ?? fallback}%，7 天 ${protection?.weeklyUsedPercentLimit ?? fallback}%`
  }
  return `账期 ${protection?.usedPercentLimit ?? 90}%`
}

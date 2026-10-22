import type { AccountQuotaProtection } from '@/api'

export function supportsQuotaProtection(value: unknown): value is AccountQuotaProtection {
  if (!value || typeof value !== 'object')
    return false
  const candidate = value as Partial<AccountQuotaProtection>
  return typeof candidate.enabled === 'boolean'
    && Number.isInteger(candidate.usedPercentLimit)
    && candidate.usedPercentLimit! >= 1 && candidate.usedPercentLimit! <= 99
}

export function parseQuotaProtection(enabled: boolean, input: string): AccountQuotaProtection | null {
  const value = input.trim()
  if (!/^\d+$/.test(value))
    return null
  const usedPercentLimit = Number(value)
  return usedPercentLimit >= 1 && usedPercentLimit <= 99 ? { enabled, usedPercentLimit } : null
}

export function quotaProtectionMatches(actual: unknown, expected: AccountQuotaProtection) {
  return supportsQuotaProtection(actual)
    && actual.enabled === expected.enabled && actual.usedPercentLimit === expected.usedPercentLimit
}

import type { Account, AccountPersonalInfoResponse } from '@/api'

export type AccountAttention = '' | 'expiring' | 'low_quota' | 'reauthorize' | 'subscription_unknown'

export function accountNeedsAttention(account: Account, filter: AccountAttention, subscription?: AccountPersonalInfoResponse['subscription'], now = Date.now()) {
  if (filter === 'reauthorize')
    return account.enabled && (['account_unverified', 'credential_expired', 'credential_invalid'].includes(account.errorReason ?? '') || (account.errorReason === 'access_token_expired' && !account.hasRefreshToken))
  if (filter === 'low_quota')
    return account.enabled && (account.quota.limitReached || account.status === 'quota_exhausted' || account.quota.windows.some(window => window.limitReached || (window.usedPercent !== null && window.usedPercent >= 90)))
  if (filter === 'expiring') {
    const expiry = subscription ? Date.parse(subscription.expiresAt) : Number.NaN
    return account.enabled && Number.isFinite(expiry) && expiry > now && expiry <= now + 3 * 86_400_000
  }
  if (filter === 'subscription_unknown')
    return account.enabled && account.provider === 'openai' && account.authenticationKind === 'oauth' && (!subscription || !Number.isFinite(Date.parse(subscription.expiresAt)))
  return true
}

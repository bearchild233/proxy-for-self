import type { AccountCreditBalance } from '@/api/modules/accounts'

export function creditBalanceDisplay(credits?: AccountCreditBalance | null) {
  if (credits?.unlimited)
    return { balance: '不限', usd: null }
  const number = (value: string | null | undefined) => {
    if (value == null || !/^\d+(?:\.\d+)?$/.test(value))
      return null
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  const balance = number(credits?.balance)
  const usd = balance == null ? null : number(credits?.usdEquivalent)
  return {
    balance: balance == null ? '未提供' : balance.toLocaleString('en-US', { maximumFractionDigits: 10 }),
    usd: usd == null ? null : usd.toLocaleString('en-US', { style: 'currency', currency: 'USD' }),
  }
}

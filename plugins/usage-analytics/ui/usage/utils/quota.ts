import type { UsageKeyAccountBreakdownItem } from '@/api'

/** 兼容旧接口的单币种字段；新接口明确返回空 costs 时不回退。 */
export function quotaCosts(item: UsageKeyAccountBreakdownItem) {
  return item.costs ?? (item.costAmount != null && item.costCurrency
    ? [{ currency: item.costCurrency, amount: item.costAmount }]
    : [])
}

/** 图表只比较同一种币种，浮点数仅用于展示比例。 */
export function quotaAmount(item: UsageKeyAccountBreakdownItem, currency: string) {
  return quotaCosts(item).reduce((sum, cost) => {
    const amount = Number(cost.amount)
    return cost.currency === currency && Number.isFinite(amount) && amount > 0 ? sum + amount : sum
  }, 0)
}

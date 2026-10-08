import type { getAccounts } from '@/api'
export type AccountQuotaWindow = Awaited<ReturnType<typeof getAccounts>>['items'][number]['quota']['windows'][number]

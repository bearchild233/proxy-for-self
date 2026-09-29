interface RefreshableAccount {
  id: string
  name: string
  enabled: boolean
  provider: string
  authenticationKind: string
}

// 只查询当前页可用的额度接口；不刷新令牌、不清除冻结、不启用停用账号。
export async function refreshAccountPage(
  accounts: readonly RefreshableAccount[],
  refresh: (id: string) => Promise<unknown>,
  signal: AbortSignal,
) {
  const eligible = accounts.filter(account => account.enabled && (
    (account.provider === 'openai' && account.authenticationKind === 'oauth')
    || account.provider === 'xai'
  ))
  let index = 0
  let succeeded = 0
  const failed: string[] = []
  async function worker() {
    while (!signal.aborted) {
      const account = eligible[index++]
      if (!account)
        return
      try {
        await refresh(account.id)
        succeeded += 1
      }
      catch {
        if (!signal.aborted)
          failed.push(account.name)
      }
    }
  }
  await Promise.all([worker(), worker()])
  return { succeeded, failed, skipped: accounts.length - eligible.length }
}

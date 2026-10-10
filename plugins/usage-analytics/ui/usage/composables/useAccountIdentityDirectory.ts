import { usePluginPolling } from '@sdk/polling'
import { onMounted, onScopeDispose, shallowRef } from 'vue'
import { getAccounts } from '@/api'

/** 只读取本地账号目录，不刷新凭据或访问上游；归档账号也保留展示名称。 */
export function useAccountIdentityDirectory() {
  const identities = shallowRef(new Map<string, { name: string, email: string | null }>())
  let controller: AbortController | undefined
  let disposed = false
  let pending = false
  async function refresh() {
    if (pending || disposed)
      return
    pending = true
    controller = new AbortController()
    const signal = controller.signal
    try {
      const next = new Map<string, { name: string, email: string | null }>()
      for (const archived of [false, true]) {
        let page = 1
        let totalPages = 1
        do {
          const result = await getAccounts({ page, pageSize: 200, archived }, { signal, silent: true })
          if (disposed || signal.aborted)
            return
          for (const account of result.items) {
            next.set(account.id, { name: account.name, email: account.email })
            next.set(account.resourceRef, { name: account.name, email: account.email })
          }
          totalPages = result.page.totalPages
          page++
        } while (page <= totalPages)
      }
      identities.value = next
    }
    catch { /* 目录暂时不可达时保留已有名称与历史快照。 */ }
    finally { pending = false }
  }
  onMounted(() => {
    void refresh()
  })
  usePluginPolling(refresh, shallowRef(30_000))
  onScopeDispose(() => {
    disposed = true
    controller?.abort()
  })
  return identities
}

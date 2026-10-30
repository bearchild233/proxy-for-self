import type { AccountGroupRef } from '@/api/modules/account-groups'
import { onMounted, onScopeDispose, shallowRef } from 'vue'
import { getAccountGroups } from '@/api/modules/account-groups'

export function useWarmupGroups() {
  const groups = shallowRef<AccountGroupRef[]>([])
  const loading = shallowRef(false)
  const error = shallowRef('')
  const controller = new AbortController()
  let sequence = 0
  let disposed = false
  async function refresh() {
    const current = ++sequence
    loading.value = true
    try {
      const first = await getAccountGroups({ page: 1, pageSize: 200 }, { silent: true, signal: controller.signal })
      const items = [...first.items]
      for (let page = 2; page <= first.page.totalPages; page++) {
        controller.signal.throwIfAborted()
        const result = await getAccountGroups({ page, pageSize: first.page.pageSize }, { silent: true, signal: controller.signal })
        items.push(...result.items)
      }
      if (current === sequence && !disposed) {
        groups.value = [...new Map(items.map(item => [item.id, item])).values()]
        error.value = ''
      }
    }
    catch (cause) {
      if (current === sequence && !disposed)
        error.value = cause instanceof Error ? cause.message : '分组目录读取失败，已保留原有选择'
    }
    finally {
      if (current === sequence)
        loading.value = false
    }
  }
  onMounted(refresh)
  onScopeDispose(() => {
    disposed = true
    sequence++
    controller.abort()
  })
  return { groups, loading, error, refresh }
}

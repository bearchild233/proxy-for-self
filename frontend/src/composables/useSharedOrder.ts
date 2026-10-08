import type { Ref } from 'vue'
import { shallowRef, watch } from 'vue'
import request from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { applyRowOrder } from '@/utils/rowOrder'

export interface RowReorder { originalIds: string[], orderedIds: string[] }

export function useSharedOrder<Row extends { id: string }>(scope: 'accounts' | 'groups' | 'keys' | 'proxies', reload: () => Promise<unknown>, rows: Ref<Row[]>) {
  const savingOrder = shallowRef(false)
  let pendingIds: string[] | undefined
  // 保存期间的后台刷新可以更新行内容，但不将拖动后的顺序跳回旧位置。
  watch(rows, (value) => {
    if (pendingIds)
      rows.value = applyRowOrder(value, pendingIds)
  }, { flush: 'sync' })
  async function saveOrder(change: RowReorder) {
    if (savingOrder.value)
      return
    savingOrder.value = true
    pendingIds = change.orderedIds
    rows.value = applyRowOrder(rows.value, change.orderedIds)
    try {
      await request({ url: '/api/admin/settings/display-order', method: 'POST', data: { scope, ...change }, silent: true })
    }
    catch {
      pendingIds = undefined
      rows.value = applyRowOrder(rows.value, change.originalIds)
      toast.error('顺序未能保存，正在同步服务器，请稍后重试')
      try {
        await reload()
      }
      catch {
        // 读取失败的提示由列表自身处理；保留恢复后的本地顺序。
      }
    }
    finally {
      pendingIds = undefined
      savingOrder.value = false
    }
  }
  return { savingOrder, saveOrder }
}

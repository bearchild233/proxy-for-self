import { shallowRef } from 'vue'
import request from '@/api/request'
import { toast } from '@/components/base/BaseToast'

export interface RowReorder { originalIds: string[], orderedIds: string[] }

export function useSharedOrder(scope: 'accounts' | 'groups' | 'keys' | 'proxies', reload: () => Promise<unknown>) {
  const savingOrder = shallowRef(false)
  async function saveOrder(change: RowReorder) {
    if (savingOrder.value)
      return
    savingOrder.value = true
    try {
      await request({ url: '/api/admin/settings/display-order', method: 'POST', data: { scope, ...change }, silent: true })
      toast.success('展示顺序已保存，全站共享')
    }
    catch {
      toast.error('顺序保存失败或已被其他管理员修改，已重新加载，请重试')
    }
    finally {
      try {
        await reload()
      }
      finally {
        savingOrder.value = false
      }
    }
  }
  return { savingOrder, saveOrder }
}

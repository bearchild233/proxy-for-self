import type { Ref } from 'vue'
import { ref } from 'vue'
import { batchUpdateAccounts } from '@/api'
import { toast } from '@/components/base/BaseToast'

/**
 * 归档/临期优先等生命周期操作。
 *
 * 这些函数直接作为 @click 处理器使用：Vue 会把处理器返回的 rejected Promise 交给
 * 父级 onErrorCaptured，而宿主 PluginFrame 在那里会把整页换成“插件运行失败”。
 * 接口错误已由请求层提示，所以这里必须吞掉异常，只负责收尾状态。
 */
export function useAccountLifecycle(options: {
  selectedIds: Ref<Set<string>>
  reload: () => Promise<unknown>
}) {
  const saving = ref(new Set<string>())
  const batchRestoring = ref(false)

  async function update(account: { id: string }, changes: { expiryPriority?: boolean, restoreArchived?: boolean }) {
    saving.value.add(account.id)
    try {
      await batchUpdateAccounts({ accountIds: [account.id], ...changes })
      toast.success(changes.restoreArchived ? '已恢复到主列表，重新授权后可启用' : '临期优先已保存')
      if (changes.restoreArchived)
        options.selectedIds.value.delete(account.id)
      await options.reload()
    }
    catch {
      // 接口错误已由请求层提示。
    }
    finally {
      saving.value.delete(account.id)
    }
  }

  /** 归档视图批量恢复：恢复后账号保持停用，需重新授权后再启用。 */
  async function restoreSelected() {
    const accountIds = [...options.selectedIds.value]
    if (accountIds.length === 0 || batchRestoring.value)
      return
    batchRestoring.value = true
    try {
      await batchUpdateAccounts({ accountIds, restoreArchived: true })
      options.selectedIds.value = new Set()
      toast.success(`已恢复 ${accountIds.length} 个账号到主列表，重新授权后可启用`)
      await options.reload()
    }
    catch {
      // 选中项保留以便重试。
    }
    finally {
      batchRestoring.value = false
    }
  }

  return { saving, batchRestoring, update, restoreSelected }
}

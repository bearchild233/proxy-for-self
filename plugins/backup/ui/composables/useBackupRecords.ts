import type { BackupRecord, BackupStatus } from '@/api'
import { usePluginPolling } from '@sdk/polling'
import { host } from '@sdk/ui'

import { computed, shallowRef } from 'vue'
import {
  createBackup,
  deleteBackup,
  getBackupRecords,
} from '@/api'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'

const ACTIVE_STATUSES: BackupStatus[] = ['queued', 'dumping', 'uploading']
const POLL_INTERVAL_MS = 2000

function isActive(status: BackupStatus): boolean {
  return ACTIVE_STATUSES.includes(status)
}

/** 备份记录：分页、可见时轮询、手动创建、下载与请求删除。 */
export function useBackupRecords() {
  const creating = shallowRef(false)
  const refreshing = shallowRef(false)
  const deleting = shallowRef(false)
  const deleteTarget = shallowRef<BackupRecord | null>(null)
  const downloadStates = shallowRef<Record<string, boolean>>({})

  const paged = usePagedQuery<{
    items: BackupRecord[]
    page: { page: number, pageSize: number, total: number, totalPages: number }
  }>({
    initialPageSize: 10,
    load: ({ page, pageSize }, options) =>
      getBackupRecords({ page, pageSize }, options),
  })

  const records = computed<BackupRecord[]>(() => paged.items.value as BackupRecord[])
  const activeBackup = computed(() =>
    records.value.some(record => isActive(record.status)),
  )

  const polling = shallowRef(false)
  function startPolling() {
    polling.value = true
  }
  function stopPolling() {
    polling.value = false
  }
  usePluginPolling(async () => {
    if (!paged.pending.value && !creating.value && !deleting.value && !refreshing.value)
      await paged.execute({ silent: true })
  }, computed(() => polling.value ? activeBackup.value ? POLL_INTERVAL_MS : 30_000 : 0))

  async function load(): Promise<void> {
    await paged.execute()
  }

  async function refresh(): Promise<void> {
    if (refreshing.value)
      return
    refreshing.value = true
    try {
      await paged.execute({ background: true })
    }
    finally {
      refreshing.value = false
    }
  }

  async function changePage(page: number): Promise<void> {
    paged.page.value = page
    await paged.execute()
  }

  async function changePageSize(pageSize: number): Promise<void> {
    paged.pageSize.value = pageSize
    paged.page.value = 1
    await paged.execute()
  }

  async function create(): Promise<void> {
    if (creating.value)
      return
    creating.value = true
    try {
      await createBackup()
      toast.success('备份任务已创建')
      await paged.execute({ silent: true })
    }
    catch {}
    finally {
      creating.value = false
    }
  }

  async function downloadBackup(record: BackupRecord): Promise<void> {
    if (downloadStates.value[record.id])
      return
    downloadStates.value = { ...downloadStates.value, [record.id]: true }
    try {
      await host('backup-download', { backupId: record.id })
    }
    catch (cause) {
      if (!(cause instanceof ApiError))
        toast.error(errorMessage(cause, '下载备份失败'))
    }
    finally {
      downloadStates.value = { ...downloadStates.value, [record.id]: false }
    }
  }

  function requestDelete(record: BackupRecord): void {
    deleteTarget.value = record
  }

  async function confirmDelete(): Promise<void> {
    const target = deleteTarget.value
    if (!target || deleting.value)
      return
    deleting.value = true
    try {
      await deleteBackup({ backupId: target.id })
      toast.success('已请求删除备份')
      await paged.execute({ silent: true })
    }
    catch {}
    finally {
      deleting.value = false
      deleteTarget.value = null
    }
  }

  return {
    records,
    page: paged.page,
    pageSize: paged.pageSize,
    total: paged.total,
    loading: paged.loading,
    error: paged.error,
    activeBackup,
    creating,
    refreshing,
    deleting,
    deleteTarget,
    downloadStates,
    load,
    refresh,
    changePage,
    changePageSize,
    create,
    downloadBackup,
    requestDelete,
    confirmDelete,
    startPolling,
    stopPolling,
  }
}

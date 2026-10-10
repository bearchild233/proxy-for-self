import type { OAuthExportFormat } from '../utils/oauthExport'
import type { Account } from '@/api'
import { exportAccounts } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage } from '@/utils/async'
import { accountOAuthExport, oauthExportFileName } from '../utils/oauthExport'

export function useAccountOAuthExport() {
  const exporting = useIdSet<string>()
  const { downloadJson } = useDownload()
  async function exportOAuth(account: Account, format: OAuthExportFormat) {
    if (account.authenticationKind !== 'oauth')
      return
    await exporting.run(account.id, async () => {
      try {
        const payload = await exportAccounts({ accountIds: account.id, confirm: 'export_sensitive_accounts' })
        await downloadJson(accountOAuthExport(payload, account, format), oauthExportFileName(account, format))
        toast.success(format === 'codex' ? '已导出 Codex auth.json' : '已导出 OAuth JSON')
      }
      catch (error) {
        toast.error(errorMessage(error, 'OAuth 导出失败'))
      }
    })
  }
  return { exportingOAuthIds: exporting.ids, exportOAuth }
}

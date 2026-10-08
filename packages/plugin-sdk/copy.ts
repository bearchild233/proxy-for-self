import { host } from './ui'
import { toast } from '../../frontend/src/components/base/BaseToast'
export function useCopyText() {
  return async (value: string, options: { successText: string, emptyErrorText?: string, errorFromException?: boolean }) => {
    if (!value) { if (options.emptyErrorText) toast.error(options.emptyErrorText); return }
    try { await host('copy', { text: value }); toast.success(options.successText) }
    catch { toast.error('复制失败') }
  }
}

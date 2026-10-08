import type { Ref, ShallowRef } from 'vue'
import type { getApiKeys } from '@/api'
import { host } from '@sdk/ui'
import { computed, shallowRef, watch } from 'vue'
import { getApiKeyBindings } from '@/api'

import { toast } from '@/components/base/BaseToast'
import { buildCodexCcSwitchImportDeeplink } from '@/utils/ccswitchImport'
import { resolveServiceRootUrl } from '@/utils/serviceUrl'

// “使用密钥”弹窗展示明文时，在列表行上补挂 reveal 得到的完整 key。
type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number] & { key?: string }

// 密钥使用与 CCSwitch 导入编排：服务根地址推导、deeplink 跳转、
// “使用密钥”弹窗的明文补全与打开。
export function useApiKeyUse(options: {
  createdKey: Readonly<Ref<string>>
  createdKeyName: Readonly<Ref<string>>
  createdKeyExcelEnabled: Readonly<Ref<boolean>>
  revealPlaintextKey: (apiKey: ApiKeyRow) => Promise<string | undefined>
}) {
  const showUseKeyModal = shallowRef(false)
  const selectedUseKey: ShallowRef<ApiKeyRow | null> = shallowRef(null)

  const serviceRootUrl = computed(() => resolveServiceRootUrl())
  const openAiBaseUrl = computed(() => `${serviceRootUrl.value}/v1`)
  const pendingImports = new Set<string>()

  async function importKey(key: string, name: string) {
    await host('external', { url: buildCodexCcSwitchImportDeeplink({
      apiKey: key,
      baseUrl: openAiBaseUrl.value,
      providerName: name,
    }) })
  }

  function importCreatedKeyToCcs() {
    if (options.createdKeyExcelEnabled.value) {
      toast.error('Excel Key 请从使用密钥面板下载完整配置与模型目录')
      return
    }
    if (!options.createdKey.value)
      return

    void importKey(options.createdKey.value, options.createdKeyName.value)
      .catch(() => toast.error('无法打开 CC Switch，请确认已安装客户端'))
  }

  async function openUseKeyModal(apiKey: ApiKeyRow) {
    const bindings = await getApiKeyBindings([apiKey.id])
    const binding = bindings.items.find(item => item.id === apiKey.id)
    const key = await options.revealPlaintextKey(apiKey)
    if (!key)
      return
    selectedUseKey.value = { ...apiKey, key, routing: binding?.routing, boundAccountId: binding?.accountId ?? null, excelBridgeEnabled: binding?.excelBridgeEnabled ?? false }
    showUseKeyModal.value = true
  }

  async function importToCcs(apiKey: ApiKeyRow) {
    if (pendingImports.has(apiKey.id))
      return
    pendingImports.add(apiKey.id)
    try {
      const bindings = await getApiKeyBindings([apiKey.id])
      const binding = bindings.items.find(item => item.id === apiKey.id)
      if (binding?.excelBridgeEnabled) {
        toast.error('Excel Key 请从使用密钥面板下载完整配置与模型目录')
        return
      }
      if (!binding?.accountId && !binding?.routing) {
        toast.error('此 Key 未绑定账号，暂不能导入配置')
        return
      }
      const key = await options.revealPlaintextKey(apiKey)
      if (key)
        await importKey(key, apiKey.name)
    }
    catch {
      toast.error('导入未完成，请重试并确认已安装 CC Switch')
    }
    finally {
      pendingImports.delete(apiKey.id)
    }
  }

  watch(showUseKeyModal, (open) => {
    if (!open)
      selectedUseKey.value = null
  })

  return {
    showUseKeyModal,
    selectedUseKey,
    serviceRootUrl,
    openAiBaseUrl,
    importCreatedKeyToCcs,
    openUseKeyModal,
    importToCcs,
  }
}

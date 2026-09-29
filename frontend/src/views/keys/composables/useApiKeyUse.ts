import type { Ref, ShallowRef } from 'vue'
import type { getApiKeys } from '@/api'
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

  function importCreatedKeyToCcs() {
    if (options.createdKeyExcelEnabled.value) {
      toast.error('Excel Key 请从使用密钥面板下载完整配置与模型目录')
      return
    }
    if (!options.createdKey.value)
      return

    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: options.createdKey.value,
      baseUrl: openAiBaseUrl.value,
      providerName: options.createdKeyName.value || 'codex-proxy-rs',
    })
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
    const bindings = await getApiKeyBindings([apiKey.id])
    const binding = bindings.items.find(item => item.id === apiKey.id)
    if ((!binding?.accountId && !binding?.routing) || binding?.excelBridgeEnabled) {
      await openUseKeyModal(apiKey)
      return
    }
    const key = await options.revealPlaintextKey(apiKey)
    if (!key)
      return
    window.location.href = buildCodexCcSwitchImportDeeplink({
      apiKey: key,
      baseUrl: openAiBaseUrl.value,
      providerName: apiKey.name || apiKey.prefix || 'codex-proxy-rs',
    })
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

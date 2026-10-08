<script setup lang="ts">
import { Apple, Copy, Download, Monitor, Upload } from '@lucide/vue'
import { host } from '@sdk/ui'
import { computed, shallowRef, watch } from 'vue'
import { getClientModels } from '@/api/modules/client-models'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import KeyCapabilityNotice from '@/components/KeyCapabilityNotice.vue'
import { useDownload } from '@/composables/useDownload'
import { excelAvailable, refreshPlugins } from '@/plugins/catalog'
import PluginSlot from '@/plugins/PluginSlot.vue'
import { buildCodexCcSwitchImportDeeplink } from '@/utils/ccswitchImport'
import {
  buildCodexConfigFiles,
  CODEX_DEFAULT_MODEL,
  CODEX_EXCEL_DEFAULT_MODEL,
  CODEX_WEBSOCKET_ENABLED_BY_DEFAULT,
} from '@/utils/codexConfig'

const props = withDefaults(defineProps<{
  title?: string
  apiKey: {
    name?: string
    key?: string
    routing?: { mode: string } | null
    boundAccountId?: string | null
    excelBridgeEnabled?: boolean
  } | null
  apiBaseUrl: string
}>(), { title: '使用密钥' })

const emit = defineEmits<{
  copy: [text: string]
}>()

const open = defineModel<boolean>({ default: false })

const activePlatform = shallowRef('unix')
const websocketEnabled = shallowRef(CODEX_WEBSOCKET_ENABLED_BY_DEFAULT)
const { downloadText } = useDownload()
const excelEnabled = computed(() => props.apiKey?.excelBridgeEnabled === true)
const pluginUnavailable = computed(() => excelEnabled.value && !excelAvailable.value)
watch(open, (active) => {
  if (active)
    void refreshPlugins()
})
const bound = computed(() => Boolean(props.apiKey?.boundAccountId || props.apiKey?.routing))
const selectedModel = shallowRef(CODEX_DEFAULT_MODEL)
watch([open, excelEnabled, () => props.apiKey], () => {
  if (open.value)
    selectedModel.value = excelEnabled.value ? CODEX_EXCEL_DEFAULT_MODEL : CODEX_DEFAULT_MODEL
})

const platformOptions = [
  { label: 'macOS / Linux', value: 'unix', icon: Apple },
  { label: 'Windows', value: 'windows', icon: Monitor },
]

const keyValue = computed(() => props.apiKey?.key ?? '')
const modelOptions = shallowRef<{ label: string, value: string }[]>([])
const modelsLoading = shallowRef(false)
const modelLoadError = shallowRef(false)
const modelReload = shallowRef(0)
watch([open, keyValue, excelEnabled, pluginUnavailable, () => props.apiBaseUrl, modelReload], async ([isOpen], _, onCleanup) => {
  modelOptions.value = []
  modelLoadError.value = false
  modelsLoading.value = false
  if (!isOpen || !keyValue.value || pluginUnavailable.value)
    return
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), 15000)
  let active = true
  onCleanup(() => {
    active = false
    clearTimeout(timeout)
    controller.abort()
  })
  modelsLoading.value = true
  try {
    const models = await getClientModels(props.apiBaseUrl, keyValue.value, controller.signal)
    if (!active)
      return
    modelOptions.value = models.map(model => ({ label: model, value: model }))
    const preferred = excelEnabled.value ? CODEX_EXCEL_DEFAULT_MODEL : CODEX_DEFAULT_MODEL
    selectedModel.value = models.includes(preferred) ? preferred : models[0] || preferred
    modelLoadError.value = models.length === 0
  }
  catch {
    if (active)
      modelLoadError.value = true
  }
  finally {
    clearTimeout(timeout)
    if (active)
      modelsLoading.value = false
  }
}, { immediate: true })
const exportModelOptions = computed(() => modelOptions.value.length
  ? modelOptions.value
  : [{ label: `${selectedModel.value}（默认配置）`, value: selectedModel.value }])
const configPath = computed(() =>
  activePlatform.value === 'windows'
    ? '%userprofile%\\.codex\\config.toml'
    : '~/.codex/config.toml',
)
const authPath = computed(() =>
  activePlatform.value === 'windows' ? '%userprofile%\\.codex\\auth.json' : '~/.codex/auth.json',
)
const codexConfigFiles = computed(() => buildCodexConfigFiles({
  apiKey: keyValue.value,
  baseUrl: props.apiBaseUrl,
  websocketEnabled: websocketEnabled.value,
  excelBridgeEnabled: excelEnabled.value,
  model: selectedModel.value,
}))

const visibleFiles = computed(() => {
  if (!bound.value || pluginUnavailable.value)
    return []
  const files = [
    { path: configPath.value, name: 'config.toml', content: codexConfigFiles.value.configToml, scrollbarHeight: '360px' },
    { path: authPath.value, name: 'auth.json', content: codexConfigFiles.value.authJson, scrollbarHeight: undefined },
  ]
  if (codexConfigFiles.value.catalogJson)
    files.push({ path: 'excel-models.json · 与 config.toml 同目录', name: 'excel-models.json', content: codexConfigFiles.value.catalogJson, scrollbarHeight: '240px' })
  files.push({ path: '使用说明.txt · 能力与限制', name: '使用说明.txt', content: codexConfigFiles.value.capabilityReadme, scrollbarHeight: '240px' })
  files.push({ path: 'Hermes 配置片段 · 请合并，不覆盖整份配置', name: 'hermes-config.yaml', content: codexConfigFiles.value.hermesConfigYaml, scrollbarHeight: '240px' })
  return files
})

function importToCcs() {
  if (!keyValue.value || excelEnabled.value || !bound.value)
    return
  void host('external', { url: buildCodexCcSwitchImportDeeplink({
    apiKey: keyValue.value,
    baseUrl: props.apiBaseUrl,
    providerName: props.apiKey?.name || 'codex-proxy-rs',
    websocketEnabled: websocketEnabled.value,
    model: selectedModel.value,
  }) })
}
</script>

<template>
  <div class="flex h-full min-h-0 flex-1 flex-col overflow-hidden">
    <BaseScrollbar class="min-h-0 flex-1">
      <div class="pr-3 pb-4">
        <p v-if="pluginUnavailable" class="text-cp-sm text-cp-text-secondary">
          此密钥绑定的服务已停用，暂不能导出配置。
        </p>
        <div v-else class="flex flex-col gap-5">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <BaseSegmented v-model="activePlatform" label="配置平台" :options="platformOptions" />
            <BaseSwitch
              v-if="!excelEnabled"
              v-model="websocketEnabled"
              label="切换 WebSocket 配置"
              active-text="WS"
              inactive-text="WS"
              inline-prompt
              :width="56"
            />
          </div>
          <p class="text-cp-sm text-cp-text-secondary">
            {{ !bound ? '此 Key 未绑定账号，暂不能导出配置' : excelEnabled ? 'Excel Bridge · 保存配置、认证与模型目录三个文件，同时提供能力说明' : 'Codex Native · 仅导出此 Key，不包含账号登录凭据' }}
          </p>
          <KeyCapabilityNotice v-if="bound" :excel-enabled="excelEnabled" />
          <div v-if="bound" class="grid gap-2">
            <BaseFormItem label="导出模型">
              <BaseSelect
                v-model="selectedModel"
                class="w-full"
                aria-label="导出模型"
                :options="exportModelOptions"
                :disabled="modelsLoading"
                searchable
                wrap-options
              />
            </BaseFormItem>
            <p class="m-0 text-cp-xs text-cp-text-secondary">
              {{ modelsLoading ? '正在加载此密钥的可用模型…' : modelLoadError ? '暂未获取到可用模型，当前保留默认配置，可重试加载。' : '选择此密钥可用的模型，支持输入关键词筛选。' }}
            </p>
            <BaseButton v-if="modelLoadError" variant="secondary" size="sm" @click="modelReload++">
              重新加载模型
            </BaseButton>
            <p class="m-0 text-cp-xs text-cp-text-secondary">
              Hermes 使用命名 provider 与 codex_responses，普通 custom 可能仍走 Chat Completions
            </p>
          </div>

          <PluginSlot placement="key-config" />
          <div class="flex flex-col gap-3">
            <section
              v-for="file in visibleFiles"
              :key="file.path"
              class="overflow-hidden rounded-cp-card bg-cp-fill-quaternary shadow-cp-tertiary"
            >
              <div class="flex items-center justify-between gap-3 px-4 py-2.5">
                <span
                  class="min-w-0 truncate font-mono text-cp-sm font-emphasis text-cp-text-secondary"
                >
                  {{ file.path }}
                </span>
                <div class="flex items-center gap-2">
                  <BaseIconButton variant="secondary" size="sm" label="下载" @click="downloadText(file.content, file.name)">
                    <Download class="size-3.5" />
                  </BaseIconButton>
                  <BaseIconButton
                    variant="secondary"
                    size="sm"
                    label="复制"
                    @click="emit('copy', file.content)"
                  >
                    <Copy class="size-3.5" />
                  </BaseIconButton>
                </div>
              </div>
              <BaseScrollbar
                :height="file.scrollbarHeight"
                max-height="360px"
              >
                <div class="mx-3 mb-3 rounded-cp bg-cp-bg-container px-3.5 py-3 shadow-cp-tertiary">
                  <pre
                    class="m-0 whitespace-pre-wrap wrap-break-word font-mono text-cp-sm leading-[1.65] font-emphasis text-cp-text"
                    v-text="file.content"
                  />
                </div>
              </BaseScrollbar>
            </section>
          </div>
        </div>
      </div>
    </BaseScrollbar>
    <div class="flex shrink-0 justify-end gap-3 border-t border-cp-border-secondary bg-cp-bg-container pt-4">
      <BaseButton v-if="!excelEnabled" variant="secondary" :disabled="!keyValue || !bound" @click="importToCcs">
        <Upload class="size-4" />
        导入 CCSwitch
      </BaseButton>
      <BaseButton variant="primary" @click="host('close', {})">
        关闭
      </BaseButton>
    </div>
  </div>
</template>

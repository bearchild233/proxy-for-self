import excelModelCatalog from './excelModelCatalog.json' with { type: 'json' }
import { buildCapabilityReadme } from './keyCapabilities.ts'

export const CODEX_DEFAULT_MODEL = 'gpt-5.6-terra'
export const CODEX_EXCEL_DEFAULT_MODEL = 'gpt-5.6-sol-excel'
export const CODEX_WEBSOCKET_ENABLED_BY_DEFAULT = false

export interface CodexConfigInput {
  apiKey: string
  baseUrl: string
  websocketEnabled?: boolean
  excelBridgeEnabled?: boolean
  model?: string
}

export function buildCodexConfigFiles(input: CodexConfigInput) {
  const baseUrl = input.baseUrl.replace(/\/+$/, '')
  const excelEnabled = input.excelBridgeEnabled === true
  const websocketEnabled = !excelEnabled && (input.websocketEnabled ?? CODEX_WEBSOCKET_ENABLED_BY_DEFAULT)
  const model = input.model?.trim() || (excelEnabled ? CODEX_EXCEL_DEFAULT_MODEL : CODEX_DEFAULT_MODEL)
  // 保留 auth.json 载荷，兼容仍依赖该字段的 CCSwitch 导入器。
  const auth = { OPENAI_API_KEY: input.apiKey }
  const configToml = `model_provider = "OpenAI"
model = ${JSON.stringify(model)}
review_model = ${JSON.stringify(model)}
${excelEnabled ? 'model_catalog_json = "./excel-models.json"\nmodel_reasoning_effort = "medium"' : '# Native reasoning uses the official model catalog default'}
service_tier = "default"

[model_providers.OpenAI]
name = "OpenAI"
base_url = ${JSON.stringify(baseUrl)}
wire_api = "responses"
supports_websockets = ${websocketEnabled}
requires_openai_auth = false
# 代理密钥仅用于网关鉴权，真实账号登录状态由服务端管理。
experimental_bearer_token = ${JSON.stringify(input.apiKey)}

[model_providers.OpenAI.http_headers]
# 声明服务端托管认证，让官方客户端启用原生生图；该标记不是密钥。
X-OpenAI-Actor-Authorization = "${excelEnabled ? 'excel-codex-bridge' : 'proxy-managed'}"

[features]
image_generation = true`

  return {
    auth,
    authJson: JSON.stringify(auth, null, 2),
    baseUrl,
    configToml,
    model,
    channel: excelEnabled ? 'excel' : 'native',
    catalogJson: excelEnabled ? JSON.stringify(excelModelCatalog, null, 2) : null,
    capabilityReadme: buildCapabilityReadme(excelEnabled),
    // 命名 provider 才会在已核对的 Hermes 版本中保留第三方域名的 Responses 模式。
    hermesConfigYaml: `# 合并到 Hermes config.yaml，不覆盖原来的其他设置与 custom_providers 条目
# 此文件含当前分发 Key，请按认证文件保管
model:
  provider: "custom:api-hub-${excelEnabled ? 'excel' : 'native'}"
  default: ${JSON.stringify(model)}
custom_providers:
  - name: "api-hub-${excelEnabled ? 'excel' : 'native'}"
    base_url: ${JSON.stringify(baseUrl)}
    api_mode: "codex_responses"
    api_key: ${JSON.stringify(input.apiKey)}
`,
  }
}

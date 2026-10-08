import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
// eslint-disable-next-line test/no-import-node-test -- 使用 Node 内置 runner，不新增测试框架依赖
import test from 'node:test'
import { buildCodexCcSwitchImportDeeplink } from '../src/utils/ccswitchImport.ts'
import { buildCodexConfigFiles, CODEX_EXCEL_DEFAULT_MODEL } from '../src/utils/codexConfig.ts'

const input = { apiKey: 'sk-test-only', baseUrl: 'https://proxy.example.invalid/v1/' }

test('CC Switch import pins provider identity independently of the display name', () => {
  const url = new URL(buildCodexCcSwitchImportDeeplink({ ...input, providerName: '我的 Key' }))
  assert.equal(url.searchParams.get('codexModelProvider'), 'custom')
  assert.equal(url.searchParams.get('codexProviderName'), 'custom')
  assert.equal(url.searchParams.get('name'), '我的 Key')
  const payload = JSON.parse(Buffer.from(url.searchParams.get('config'), 'base64').toString())
  assert.ok(payload.config.includes('model_provider = "custom"'))
  assert.ok(payload.config.includes('[model_providers.custom]'))
  assert.ok(payload.config.includes('name = "custom"'))
  assert.ok(payload.config.includes('model_reasoning_effort = "high"'))
})

test('Native export has no Excel catalog or alias', () => {
  const files = buildCodexConfigFiles(input)
  assert.equal(files.channel, 'native')
  assert.equal(files.catalogJson, null)
  assert.equal(files.baseUrl, 'https://proxy.example.invalid/v1')
  assert.ok(!files.configToml.includes('model_catalog_json'))
  assert.ok(!files.configToml.includes('-excel'))
  assert.ok(!files.configToml.includes('model_reasoning_effort = "max"'))
  assert.deepEqual(JSON.parse(files.authJson), { OPENAI_API_KEY: input.apiKey })
})

test('Excel export uses original catalog and disables unsupported transports', () => {
  const files = buildCodexConfigFiles({ ...input, excelBridgeEnabled: true, websocketEnabled: true })
  assert.equal(files.channel, 'excel')
  assert.equal(files.model, CODEX_EXCEL_DEFAULT_MODEL)
  assert.ok(files.configToml.includes('model_catalog_json = "./excel-models.json"'))
  assert.ok(files.configToml.includes('supports_websockets = false'))
  assert.ok(files.configToml.includes('image_generation = true'))
  assert.ok(files.configToml.includes('X-OpenAI-Actor-Authorization'))
  const models = JSON.parse(files.catalogJson).models
  assert.equal(models.length, 12)
  assert.equal(new Set(models.map(model => model.slug)).size, 12)
  assert.equal(models[0].slug, CODEX_EXCEL_DEFAULT_MODEL)
  for (const model of models) {
    assert.ok(model.slug.endsWith('-excel'))
    assert.equal(model.default_reasoning_level, 'medium')
    assert.deepEqual(model.supported_reasoning_levels.map(level => level.effort), ['low', 'medium', 'high', 'xhigh'])
  }
})

test('Switching one export leaves another key on Native', () => {
  const before = buildCodexConfigFiles(input)
  const other = buildCodexConfigFiles({ ...input, apiKey: 'sk-other', excelBridgeEnabled: true })
  const after = buildCodexConfigFiles(input)
  assert.deepEqual(before, after)
  assert.ok(!other.catalogJson.includes(input.apiKey))
  assert.ok(!other.catalogJson.includes('sk-other'))
})

test('Values cannot escape the exported TOML string', () => {
  const key = `sk-test"${String.fromCharCode(10)}[malicious]`
  const files = buildCodexConfigFiles({ ...input, apiKey: key })
  assert.ok(files.configToml.includes(`experimental_bearer_token = ${JSON.stringify(key)}`))
  assert.equal(JSON.parse(files.authJson).OPENAI_API_KEY, key)
})

test('Export carries transparent capabilities without leaking key material', () => {
  const files = buildCodexConfigFiles({ ...input, excelBridgeEnabled: true })
  assert.ok(files.capabilityReadme.includes('/v1/chat/completions'))
  assert.ok(files.capabilityReadme.includes('previous_response_id'))
  assert.ok(files.capabilityReadme.includes('改成说明文本继续请求'))
  assert.ok(files.capabilityReadme.includes('input_file'))
  assert.ok(files.capabilityReadme.includes('不会兑换卡片'))
  assert.ok(!files.capabilityReadme.includes(input.apiKey))
})

test('Hermes export uses a named provider with explicit Responses mode', () => {
  const files = buildCodexConfigFiles({ ...input, model: 'gpt-6-astra' })
  assert.ok(files.hermesConfigYaml.includes('provider: "custom:api-hub-native"'))
  assert.ok(files.hermesConfigYaml.includes('api_mode: "codex_responses"'))
  assert.ok(files.hermesConfigYaml.includes('default: "gpt-6-astra"'))
  assert.ok(files.configToml.includes('model = "gpt-6-astra"'))
  const excel = buildCodexConfigFiles({ ...input, excelBridgeEnabled: true })
  assert.ok(excel.hermesConfigYaml.includes('custom:api-hub-excel'))
  assert.ok(excel.hermesConfigYaml.includes(CODEX_EXCEL_DEFAULT_MODEL))
})

test('User-selected model cannot escape TOML or YAML strings', () => {
  const model = 'model"\n[malicious]'
  const files = buildCodexConfigFiles({ ...input, model })
  assert.ok(files.configToml.includes(`model = ${JSON.stringify(model)}`))
  assert.ok(files.hermesConfigYaml.includes(`default: ${JSON.stringify(model)}`))
})

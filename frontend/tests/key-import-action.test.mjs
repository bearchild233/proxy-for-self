/* eslint-disable no-new-func -- 注入受控依赖，执行实际导入编排代码 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 复用仓库内置测试 runner
import test from 'node:test'
import { computed, effectScope, shallowRef, watch } from 'vue'
import { parse } from 'vue/compiler-sfc'
import { buildCodexCcSwitchImportDeeplink } from '../src/utils/ccswitchImport.ts'

function fixture(binding = { accountId: 'test-account' }, key = 'sk-test-only') {
  const calls = []
  const errors = []
  const deps = {
    computed,
    shallowRef,
    watch,
    buildCodexCcSwitchImportDeeplink,
    host: async (...args) => {
      calls.push(args)
    },
    getApiKeyBindings: async () => ({ items: [{ id: 'test-key', ...binding }] }),
    resolveServiceRootUrl: () => 'https://example.invalid',
    toast: { error: message => errors.push(message) },
  }
  const code = stripTypeScriptTypes(readFileSync(new URL('../../plugins/keys/ui/page/composables/useApiKeyUse.ts', import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import .*$/gm, '')
    .replace('export function', 'function')
  const useApiKeyUse = new Function(...Object.keys(deps), `${code};return useApiKeyUse`)(...Object.values(deps))
  const scope = effectScope()
  const api = scope.run(() => useApiKeyUse({ createdKey: shallowRef(key), createdKeyName: shallowRef('测试 Key'), createdKeyExcelEnabled: shallowRef(false), revealPlaintextKey: async () => key }))
  return { api, calls, errors, stop: () => scope.stop() }
}
const row = { id: 'test-key', name: '测试 Key' }

test('more-menu import immediately requests CC Switch deeplink without an information popup', async () => {
  const f = fixture()
  try {
    await Promise.all([f.api.importToCcs(row), f.api.importToCcs(row)])
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][0], 'external')
    const url = new URL(f.calls[0][1].url)
    assert.equal(url.protocol, 'ccswitch:')
    assert.equal(url.searchParams.get('apiKey'), 'sk-test-only')
    assert.equal(url.searchParams.get('name'), row.name)
    assert.equal(url.searchParams.get('endpoint'), 'https://example.invalid/v1')
    assert.equal(f.api.showUseKeyModal.value, false)
  }
  finally { f.stop() }
})

test('use-key still opens information; newly-created key imports directly', async () => {
  const f = fixture()
  try {
    await f.api.openUseKeyModal(row)
    assert.equal(f.api.showUseKeyModal.value, true)
    assert.equal(f.api.selectedUseKey.value.key, 'sk-test-only')
    assert.deepEqual(f.calls, [])
    f.api.importCreatedKeyToCcs()
    assert.equal(f.calls[0][0], 'external')
  }
  finally { f.stop() }
})

test('unsupported Excel, unbound or unavailable keys never launch an invalid import', async () => {
  for (const [binding, key] of [[{ excelBridgeEnabled: true }, 'key'], [{}, 'key'], [{ accountId: 'a' }, undefined]]) {
    const f = fixture(binding, key === undefined ? '' : key)
    try {
      await f.api.importToCcs(row)
      assert.deepEqual(f.calls, [])
      assert.equal(f.api.showUseKeyModal.value, false)
    }
    finally { f.stop() }
  }
})

test('configuration actions are a non-shrinking sibling outside the scrolling content', () => {
  const source = readFileSync(new URL('../../plugins/client-access/ui/ConfigPanel.vue', import.meta.url), 'utf8')
  const descriptor = parse(source).descriptor
  const root = descriptor.template.ast.children.find(n => n.tag === 'div')
  const children = root.children.filter(n => n.type === 1)
  assert.deepEqual(children.map(n => n.tag), ['BaseScrollbar', 'div'])
  const footer = children[1]
  assert.match(footer.props.find(p => p.name === 'class').value.content, /shrink-0/)
  assert.equal(footer.children.filter(n => n.tag === 'BaseButton').length, 2)
})

test('host only accepts CC Switch imports from keys or client-access while client-access is enabled', () => {
  const source = readFileSync(new URL('../src/plugins/PluginFrame.vue', import.meta.url), 'utf8')
  const branch = source.match(/case 'external': \{([\s\S]*?)\r?\n {4}\}\r?\n {4}default:/)[1]
  const execute = new Function('input', 'bound', 'platformPlugins', 'location', branch)
  for (const id of ['keys', 'client-access']) {
    const location = {}
    execute({ url: 'ccswitch://v1/import?resource=provider' }, { id }, { value: [{ id: 'client-access' }] }, location)
    assert.equal(location.href, 'ccswitch://v1/import?resource=provider')
    assert.throws(() => execute({ url: 'https://example.invalid' }, { id }, { value: [{ id: 'client-access' }] }, {}))
    assert.throws(() => execute({ url: 'ccswitch://v1/import?x=1' }, { id }, { value: [] }, {}))
  }
  assert.throws(() => execute({ url: 'ccswitch://v1/import?x=1' }, { id: 'accounts' }, { value: [{ id: 'client-access' }] }, {}))
})

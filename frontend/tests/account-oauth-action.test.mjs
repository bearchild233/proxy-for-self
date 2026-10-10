/* eslint-disable no-new-func -- 编译真实 Vue 组件并在内存 renderer 中执行，不访问浏览器或真实凭据 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'
import * as vue from 'vue'
import { compileScript, compileTemplate, parse } from 'vue/compiler-sfc'

function source(path) {
  return readFileSync(new URL(path, import.meta.url), 'utf8')
}
function loadFunctions(path, dependencies, names) {
  const code = stripTypeScriptTypes(source(path), { mode: 'strip' })
    .replace(/^import .*$/gm, '')
    .replace(/^export /gm, '')
  return new Function(...Object.keys(dependencies), `${code}\nreturn { ${names.join(',')} }`)(...Object.values(dependencies))
}
function compileComponent(path, modules) {
  const { descriptor } = parse(source(path))
  const code = stripTypeScriptTypes(compileScript(descriptor, { id: 'oauth-action-test', inlineTemplate: true }).content, { mode: 'strip' })
    .replace(/import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]/g, (_, bindings, name) => `const {${bindings.replace(/\bas\b/g, ':')}} = modules[${JSON.stringify(name)}]`)
    .replace(/import\s+(\w+)\s+from\s+['"]([^'"]+)['"]/g, (_, binding, name) => `const ${binding} = modules[${JSON.stringify(name)}]`)
    .replace('export default', 'return')
  return new Function('modules', code)({ vue, ...modules })
}
function renderer() {
  const node = (type, text = '') => ({ type, text, props: {}, children: [], parent: null })
  const runtime = vue.createRenderer({
    createElement: type => node(type),
    createText: text => node('text', text),
    createComment: () => node('comment'),
    insert(child, parent, anchor) {
      if (child.parent)
        child.parent.children.splice(child.parent.children.indexOf(child), 1)
      child.parent = parent
      const index = anchor ? parent.children.indexOf(anchor) : -1
      parent.children.splice(index < 0 ? parent.children.length : index, 0, child)
    },
    remove(child) { child.parent?.children.splice(child.parent.children.indexOf(child), 1) },
    patchProp: (node, key, _, value) => { node.props[key] = value },
    setText: (node, text) => { node.text = text },
    setElementText: (node, text) => {
      node.text = text
      node.children = []
    },
    parentNode: node => node.parent,
    nextSibling: node => node.parent?.children[node.parent.children.indexOf(node) + 1] ?? null,
  })
  return { runtime, root: node('root') }
}
function text(node) {
  return node.text + node.children.map(text).join('')
}
function find(node, label) {
  if (node.type === 'button' && text(node).trim() === label)
    return node
  for (const child of node.children) {
    const found = find(child, label)
    if (found)
      return found
  }
}

test('real export menu clicks reach the page listener, API and host download for both formats', async () => {
  const downloads = []
  const requests = []
  const messages = []
  let resolveExport
  let closes = 0
  const account = { id: 'acct_fixture', provider: 'openai', authenticationKind: 'oauth' }
  // 与 Rust CodexCprOAuthExportAccount 的 Serialize 合同一致，OAuth 无 authenticationKind。
  const payload = { documents: [{ provider: 'openai', document: { sourceFormat: 'cpr', accounts: [{
    id: account.id,
    accountId: 'upstream-fixture',
    accessToken: 'fixture-access',
    refreshToken: 'fixture-refresh',
    idToken: 'fixture-id',
  }] } }] }
  const utils = loadFunctions('../../plugins/accounts/ui/page/utils/oauthExport.ts', { isRecord: value => value !== null && typeof value === 'object' && !Array.isArray(value) }, ['accountOAuthExport', 'oauthExportFileName'])
  const { useIdSet } = loadFunctions('../src/composables/useIdSet.ts', { shallowRef: vue.shallowRef }, ['useIdSet'])
  const { useDownload } = loadFunctions('../../packages/plugin-sdk/download.ts', {
    host: async (operation, input) => {
      downloads.push({ operation, input })
    },
  }, ['useDownload'])
  const { useAccountOAuthExport } = loadFunctions('../../plugins/accounts/ui/page/composables/useAccountOAuthExport.ts', {
    ...utils,
    useIdSet,
    useDownload,
    exportAccounts: (input) => {
      requests.push(input)
      return new Promise((resolve) => {
        resolveExport = resolve
      })
    },
    toast: { success: message => messages.push(message), error: message => messages.push(message) },
    errorMessage: error => error.message,
  }, ['useAccountOAuthExport'])
  const icon = vue.defineComponent(() => () => vue.h('i'))
  const menu = compileComponent('../src/components/base/BaseMenuItem.vue', { '@lucide/vue': { LoaderCircle: icon } })
  const actions = compileComponent('../../plugins/accounts/ui/page/components/AccountTableActions.vue', {
    '@lucide/vue': Object.fromEntries(['Download', 'KeyRound', 'MoreHorizontal', 'Pencil', 'RefreshCw', 'RotateCcw', 'Trash2', 'Wifi'].map(name => [name, icon])),
    '@/components/base/BaseIconButton.vue': vue.defineComponent((_, { slots }) => () => vue.h('button', slots.default?.())),
    '@/components/base/BaseMenuItem.vue': menu,
    '@/components/base/BasePopover.vue': vue.defineComponent((_, { slots }) => () => vue.h('div', slots.default?.({ close: () => closes++, open: true }))),
  })
  // 从实际父页面编译结果取得监听属性，避免测试沿用子组件的错误拼写。
  const template = parse(source('../../plugins/accounts/ui/page/index.vue')).descriptor.template.content
  const compiledParent = compileTemplate({ source: template.match(/<AccountTableActions[\s\S]*?\/>/)[0], id: 'oauth-parent-test' }).code
  const handlerName = compiledParent.match(/(onExport\w+):/)?.[1]
  const busyProp = compiledParent.match(/"(exporting-[^"]+)":/)?.[1]
  assert.equal(handlerName, 'onExportOauth')
  assert.equal(busyProp, 'exporting-oauth')
  const { runtime, root } = renderer()
  const app = runtime.createApp(vue.defineComponent({ setup() {
    const { exportOAuth, exportingOAuthIds } = useAccountOAuthExport()
    return () => vue.h(actions, {
      account,
      deleting: false,
      recovering: false,
      refreshing: false,
      testing: false,
      diagnosticsEnabled: false,
      [busyProp]: exportingOAuthIds.value.has(account.id),
      [handlerName]: exportOAuth,
    })
  } }))
  app.mount(root)
  try {
    for (const label of ['导出 OAuth JSON', '导出 Codex auth.json']) {
      const button = find(root, label)
      assert.ok(button, label)
      button.props.onClick({ stopPropagation() {} })
      await vue.nextTick()
      assert.equal(find(root, '导出 OAuth JSON').props.disabled, true)
      assert.equal(find(root, '导出 Codex auth.json').props.disabled, true)
      resolveExport(payload)
      await new Promise(resolve => setImmediate(resolve))
    }
    assert.equal(closes, 2)
    assert.deepEqual(requests, Array.from({ length: 2 }, () => ({ accountIds: account.id, confirm: 'export_sensitive_accounts' })))
    assert.equal(downloads.length, 2, 'each actual menu click must download once')
    assert.equal(downloads[0].operation, 'download')
    assert.deepEqual(JSON.parse(downloads[0].input.text), payload)
    assert.equal(downloads[1].input.fileName, 'auth.json')
    assert.equal(downloads[1].input.type, 'application/json;charset=utf-8')
    assert.equal(JSON.parse(downloads[1].input.text).tokens.access_token, 'fixture-access')
    assert.deepEqual(messages, ['已导出 OAuth JSON', '已导出 Codex auth.json'])
  }
  finally { app.unmount() }
})

function editorFixture(accountOverrides = {}) {
  const calls = []
  const messages = []
  const toast = { success: value => messages.push(value), warning: value => messages.push(value), error: value => messages.push(value) }
  const { useAsyncAction } = loadFunctions('../src/composables/useAsyncAction.ts', {
    shallowRef: vue.shallowRef,
    ApiError: class extends Error {},
    toast,
    errorMessage: error => error.message,
  }, ['useAsyncAction'])
  const account = { id: 'acct_fixture', authenticationKind: 'oauth', name: 'Fixture', enabled: true, weight: 50, concurrencyLimit: 2, modelAccess: { mode: 'all', models: [] }, groups: [{ id: 'group-a' }], ...accountOverrides }
  const accounts = vue.ref([account])
  const { useAccountEditor } = loadFunctions('../../plugins/accounts/ui/page/composables/useAccountEditor.ts', {
    computed: vue.computed,
    ref: vue.ref,
    shallowRef: vue.shallowRef,
    watch: vue.watch,
    useAsyncAction,
    toast,
    useRequestState: () => ({
      loading: vue.shallowRef(false),
      invalidate() {},
      start() { return 1 },
      isCurrent: () => true,
      finish() {},
      fail: (_, error) => { throw error },
    }),
    getAccountDetail: async () => ({ credentialConfiguration: { base_url: 'https://upstream.invalid', transport: 'http' } }),
    updateAccount: async input => calls.push(['update', input]),
    batchUpdateAccounts: async input => calls.push(['batch', input]),
    updateAccountApiKey: async input => calls.push(['rotate', input]),
    accountModelAccessError: () => null,
    apiKeyAccountError: () => null,
    concurrencyLimitInput: value => String(value),
    parseAccountSchedulingForm: (limit, weight) => ({ valid: true, values: { concurrencyLimit: Number(limit), weight: Number(weight) } }),
    emptyApiKeyAccountForm: () => ({ apiKey: '', base_url: '', transport: 'http' }),
  }, ['useAccountEditor'])
  const scope = vue.effectScope()
  const editor = scope.run(() => useAccountEditor({
    accounts,
    reloadAccounts: async options => calls.push(['reload', options]),
    reloadGroups: async () => calls.push(['groups']),
  }))
  editor.open(account)
  return { editor, accounts, calls, messages, scope }
}

test('unchanged normalized account closes without any write, reload or success toast', async () => {
  const fixture = editorFixture()
  try {
    fixture.editor.name.value = ' Fixture '
    fixture.editor.selectedGroupIds.value = ['group-a', 'group-a']
    await fixture.editor.save()
    assert.equal(fixture.editor.showEditModal.value, false)
    assert.deepEqual(fixture.calls, [])
    assert.deepEqual(fixture.messages, [])
  }
  finally { fixture.scope.stop() }
})

test('group-only edit writes a sparse group command and preserves concurrently refreshed settings', async () => {
  const fixture = editorFixture()
  try {
    fixture.accounts.value[0] = { ...fixture.accounts.value[0], name: 'Another administrator', weight: 100 }
    fixture.editor.selectedGroupIds.value = ['group-b']
    await fixture.editor.save()
    assert.deepEqual(fixture.calls, [
      ['batch', { accountIds: ['acct_fixture'], groupIds: ['group-b'] }],
      ['reload', { background: true }],
      ['groups'],
    ])
    assert.equal(fixture.editor.showEditModal.value, false)
  }
  finally { fixture.scope.stop() }
})

test('background list refresh cannot turn an unchanged form into an account overwrite', async () => {
  const fixture = editorFixture()
  try {
    fixture.accounts.value[0] = { ...fixture.accounts.value[0], weight: 100, groups: [{ id: 'group-b' }] }
    await fixture.editor.save()
    assert.deepEqual(fixture.calls, [])
  }
  finally { fixture.scope.stop() }
})

test('API Key account metadata keeps its credential and actual connection edits still rotate', async () => {
  for (const change of ['none', 'groups', 'connection']) {
    const fixture = editorFixture({ authenticationKind: 'api_key' })
    try {
      await new Promise(resolve => setImmediate(resolve))
      if (change === 'groups')
        fixture.editor.selectedGroupIds.value = ['group-b']
      if (change === 'connection')
        fixture.editor.apiKey.value.base_url = 'https://changed.invalid'
      await fixture.editor.save()
      assert.equal(fixture.calls[0]?.[0], { none: undefined, groups: 'batch', connection: 'rotate' }[change])
    }
    finally { fixture.scope.stop() }
  }
})

test('account save reports committed changes before background reread and skips unchanged groups', async () => {
  const messages = []
  const refreshes = []
  let resolveUpdate
  let resolveReload
  const updated = new Promise((resolve) => {
    resolveUpdate = resolve
  })
  const reloaded = new Promise((resolve) => {
    resolveReload = resolve
  })
  const toast = { success: message => messages.push(message), warning() {}, error() {} }
  const { useAsyncAction } = loadFunctions('../src/composables/useAsyncAction.ts', {
    shallowRef: vue.shallowRef,
    ApiError: class extends Error {},
    toast,
    errorMessage: error => error.message,
  }, ['useAsyncAction'])
  const { useAccountEditor } = loadFunctions('../../plugins/accounts/ui/page/composables/useAccountEditor.ts', {
    computed: vue.computed,
    ref: vue.ref,
    shallowRef: vue.shallowRef,
    watch: vue.watch,
    useAsyncAction,
    toast,
    useRequestState: () => ({ loading: vue.shallowRef(false), invalidate() {} }),
    getAccountDetail: () => { throw new Error('OAuth edits must not load API Key configuration') },
    updateAccount: () => updated,
    batchUpdateAccounts: () => { throw new Error('name edits must use the account update') },
    updateAccountApiKey: () => { throw new Error('unexpected rotation') },
    accountModelAccessError: () => null,
    apiKeyAccountError: () => null,
    concurrencyLimitInput: value => String(value),
    parseAccountSchedulingForm: () => ({ valid: true, values: { concurrencyLimit: 2, weight: 50 } }),
    emptyApiKeyAccountForm: () => ({ apiKey: '', base_url: '', transport: 'http' }),
  }, ['useAccountEditor'])
  const account = { id: 'acct_fixture', authenticationKind: 'oauth', name: 'Fixture', enabled: true, weight: 50, concurrencyLimit: 2, modelAccess: { mode: 'all', models: [] }, groups: [] }
  const scope = vue.effectScope()
  const editor = scope.run(() => useAccountEditor({
    accounts: vue.ref([account]),
    reloadAccounts: (options) => {
      refreshes.push(options)
      return reloaded
    },
    reloadGroups: () => { throw new Error('unchanged groups must not reload') },
  }))
  try {
    editor.open(account)
    editor.name.value = 'Changed name'
    const saving = editor.save()
    assert.equal(editor.showEditModal.value, true)
    assert.equal(messages.length, 0, 'never report success before the write is confirmed')
    resolveUpdate({ accountId: account.id, configRevision: 2 })
    await new Promise(resolve => setImmediate(resolve))
    assert.equal(editor.showEditModal.value, false)
    assert.equal(editor.saving.value, false, 'background reads must not keep save busy')
    assert.deepEqual(messages, ['账号已更新'])
    assert.deepEqual(refreshes, [{ background: true }])
    resolveReload(true)
    await saving
    assert.equal(editor.saving.value, false)
  }
  finally { scope.stop() }
})

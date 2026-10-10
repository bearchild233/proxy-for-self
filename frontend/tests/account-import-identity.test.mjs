/* eslint-disable no-new-func -- 执行实际导入 composable，使用受控 API，不发送真实凭据 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'
import * as Vue from 'vue'

function load(path, name, deps = {}) {
  const code = stripTypeScriptTypes(readFileSync(new URL(path, import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import\s[\s\S]*?from ['"][^'"]+['"];?\r?\n/gm, '')
    .replace(/^export /gm, '')
  const entries = Object.entries(deps).filter(([key]) => /^[a-z_$][\w$]*$/i.test(key) && key !== 'default')
  return new Function(...entries.map(([key]) => key), `${code}\nreturn ${name}`)(...entries.map(([, value]) => value))
}

const applyAccountImportName = load('../../plugins/accounts/ui/page/utils/importName.ts', 'applyAccountImportName')
const accountIdentity = load('../../plugins/usage-analytics/ui/usage/utils/accountIdentity.ts', 'accountIdentity')

test('import names cover single, array and wrapped documents without mutating credentials or source', () => {
  for (const document of [
    { accessToken: 'fixture', name: 'old' },
    { accounts: [{ accessToken: 'fixture', name: 'old' }, { refreshToken: 'fixture-rt' }] },
    { data: { accounts: [{ accessToken: 'fixture', name: 'old' }] }, version: 1 },
  ]) {
    const original = structuredClone(document)
    const result = applyAccountImportName(document, '  主力 Plus  ')
    assert.deepEqual(document, original)
    const entries = result.accounts || result.data?.accounts || [result]
    assert.ok(entries.every(entry => entry.name === '主力 Plus'))
    assert.equal(entries[0].accessToken, 'fixture')
    assert.equal(applyAccountImportName(document, '  '), document)
  }
})

test('visible identity prefers name, shows email once and never falls back to account reference', () => {
  assert.deepEqual(accountIdentity({ name: '主力 Plus', email: 'fixture@example.com', reference: 'acct_fixture' }), {
    primary: '主力 Plus',
    secondary: 'fixture@example.com',
    full: '主力 Plus · fixture@example.com',
  })
  assert.deepEqual(accountIdentity({ name: 'fixture@example.com', email: 'fixture@example.com' }), {
    primary: 'fixture@example.com',
    secondary: '',
    full: 'fixture@example.com',
  })
  assert.equal(accountIdentity({ name: 'acct_fixture', reference: 'acct_fixture' }).primary, '未知账号')
  assert.equal(accountIdentity({ email: 'fixture@example.com' }).primary, 'fixture@example.com')
})

function onboardingFixture() {
  const path = '../../plugins/accounts/ui/page/'
  const emptyApiKeyAccountForm = load(`${path}utils/upstreamApiKey.ts`, 'emptyApiKeyAccountForm')
  const parseAccountSchedulingForm = load(`${path}utils/schedulingForm.ts`, 'parseAccountSchedulingForm')
  const accountModelAccessError = load(`${path}utils/modelAccess.ts`, 'accountModelAccessError')
  const deps = { emptyApiKeyAccountForm, parseAccountSchedulingForm, accountModelAccessError }
  const emptyAccountCreateForm = load(`${path}components/AccountCreateModal/model.ts`, 'emptyAccountCreateForm', deps)
  const accountImportSettings = load(`${path}components/AccountCreateModal/model.ts`, 'accountImportSettings', deps)
  const accountProxyError = load(`${path}components/AccountCreateModal/model.ts`, 'accountProxyError', deps)
  const requests = []
  const useAccountOnboarding = load(`${path}composables/useAccountOnboarding.ts`, 'useAccountOnboarding', {
    ...Vue,
    emptyAccountCreateForm,
    accountImportSettings,
    accountProxyError,
    emptyApiKeyAccountForm,
    applyAccountImportName,
    apiKeyAccountError: () => undefined,
    useAsyncAction: () => ({ loading: Vue.ref(false), run: fn => fn() }),
    isRecord: value => value !== null && typeof value === 'object' && !Array.isArray(value),
    isSupportedProvider: provider => ['openai', 'xai'].includes(provider),
    formatProviderLabel: String,
    generateRequestId: () => 'fixture-submission',
    toast: { success() {} },
    createAccountImportTask: async (input) => {
      requests.push(input)
      return { id: 'fixture-task' }
    },
    importAccounts: async input => requests.push(input),
    startAccountOAuth: async (input) => {
      requests.push(input)
      return { flowId: 'fixture-flow', authorizationUrl: 'https://fixture.invalid' }
    },
    completeAccountOAuth: async input => requests.push(input),
  })
  const scope = Vue.effectScope()
  const state = scope.run(() => useAccountOnboarding({ reload: async () => {}, onImportTaskCreated() {} }))
  state.openCreateAccount()
  state.createForm.value.provider = 'openai'
  state.createForm.value.name = ' 主力 Plus '
  state.createForm.value.notes = ' 仅测试 '
  return { state, requests, scope }
}

test('actual token and JSON submissions carry the name, notes and existing scheduling settings together', async () => {
  for (const mode of ['access_token', 'refresh_token', 'json']) {
    const { state, requests, scope } = onboardingFixture()
    const form = state.createForm.value
    form.mode = mode
    form.groupIds = ['fixture-group']
    form.importTexts[mode] = mode === 'json' ? JSON.stringify([{ accessToken: 'fixture' }]) : 'fixture-1\nfixture-2'
    await state.handleCreate()
    const input = requests[0]
    assert.ok(input.items.every(item => item.data.accounts.every(account => account.name === '主力 Plus')))
    assert.ok(input.items.every(item => item.settings.notes === '仅测试'))
    assert.deepEqual(input.items[0].settings.groupIds, ['fixture-group'])
    assert.equal(input.items[0].settings.weight, 50)
    scope.stop()
  }
})

test('OAuth uses the entered name, completing authorization keeps notes, and reauthorization keeps existing identity', async () => {
  const { state, requests, scope } = onboardingFixture()
  await state.handleAuthorizeOAuth()
  assert.equal(requests[0].name, '主力 Plus')
  state.createForm.value.oauthCallback = 'https://fixture.invalid/callback'
  await state.handleCreate()
  assert.equal(requests[1].settings.notes, '仅测试')
  state.openReauthorizeAccount({ id: 'acct_fixture', name: 'Existing name', email: 'fixture@example.com', provider: 'openai', authenticationKind: 'oauth' })
  await Vue.nextTick()
  assert.equal(requests[2].name, 'Existing name')
  state.createForm.value.oauthCallback = 'https://fixture.invalid/callback'
  await state.handleCreate()
  assert.equal(requests[3].settings, undefined)
  scope.stop()
})

test('API Key import keeps its explicit name and includes entered notes', async () => {
  const { state, requests, scope } = onboardingFixture()
  state.createForm.value.mode = 'api_key'
  state.createForm.value.apiKey = { name: 'API account', base_url: 'https://fixture.invalid', apiKey: 'fixture', transport: 'http' }
  await state.handleCreate()
  assert.equal(requests[0].data.name, 'API account')
  assert.equal(requests[0].settings.notes, '仅测试')
  scope.stop()
})

test('identity directory reads every active and archived page, silently preserves names on failure', async () => {
  let refresh
  let dispose
  let fail = false
  const requests = []
  const useAccountIdentityDirectory = load('../../plugins/usage-analytics/ui/usage/composables/useAccountIdentityDirectory.ts', 'useAccountIdentityDirectory', {
    shallowRef: Vue.shallowRef,
    onMounted() {},
    onScopeDispose: (fn) => { dispose = fn },
    usePluginPolling: (fn) => { refresh = fn },
    getAccounts: async (params, options) => {
      requests.push(params)
      assert.equal(options.silent, true)
      if (fail)
        throw new Error('fixture unavailable')
      const id = `${params.archived}-${params.page}`
      return { items: [{ id, resourceRef: `ref-${id}`, name: `Name ${id}`, email: `${id}@example.com` }], page: { totalPages: params.archived ? 1 : 2 } }
    },
  })
  const identities = useAccountIdentityDirectory()
  await refresh()
  assert.deepEqual(requests.map(params => [params.archived, params.page]), [[false, 1], [false, 2], [true, 1]])
  assert.equal(identities.value.get('true-1').name, 'Name true-1')
  assert.equal(identities.value.get('ref-false-2').name, 'Name false-2')
  const previous = identities.value
  fail = true
  await refresh()
  assert.equal(identities.value, previous)
  dispose()
  const count = requests.length
  await refresh()
  assert.equal(requests.length, count)
})

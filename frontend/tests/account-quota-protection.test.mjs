/* eslint-disable no-new-func -- 执行真实 composable 源码并注入受控 API，验证原子保存与旧网关回读 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 复用仓库内置 runner
import test from 'node:test'
import * as Vue from 'vue'
import { parseQuotaProtection, QUOTA_PROTECTION_PRESETS, quotaProtectionMatches, supportsQuotaProtection, supportsSplitQuotaProtection } from '../../plugins/accounts/ui/page/utils/quotaProtection.ts'

function loadComposable(path, name, deps) {
  const code = stripTypeScriptTypes(readFileSync(new URL(path, import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import .*$/gm, '')
    .replace(/^export /gm, '')
  const entries = Object.entries(deps).filter(([key]) => /^[a-z_$][\w$]*$/i.test(key) && key !== 'default')
  return new Function(...entries.map(([key]) => key), `${code}; return ${name}`)(...entries.map(([, value]) => value))
}

function fixture({ protection = { enabled: false, usedPercentLimit: 90 }, ignore = false, apiKey = false, provider = 'xai', readback } = {}) {
  const calls = []
  const messages = []
  const row = { provider, id: 'test-account', name: '演示', enabled: true, weight: 50, notes: '', groups: [], concurrencyLimit: null, modelAccess: { mode: 'all', models: [] }, authenticationKind: apiKey ? 'api_key' : 'oauth', quotaProtection: protection }
  let saved = protection
  const deps = {
    ...Vue,
    parseQuotaProtection,
    quotaProtectionMatches,
    supportsQuotaProtection,
    supportsSplitQuotaProtection,
    getAccountDetail: async () => ({ account: { ...row, quotaProtection: readback ? readback(saved) : saved }, credentialConfiguration: { base_url: 'https://example.invalid', transport: 'http' } }),
    updateAccount: async (input) => {
      calls.push(['update', input])
      if (!ignore && input.quotaProtection)
        saved = input.quotaProtection
    },
    updateAccountApiKey: async (input) => {
      calls.push(['rotate', input])
      if (!ignore && input.settings.quotaProtection)
        saved = input.settings.quotaProtection
    },
    batchUpdateAccounts: async input => calls.push(['batch', input]),
    toast: Object.fromEntries(['success', 'warning', 'error'].map(kind => [kind, message => messages.push([kind, message])])),
    concurrencyLimitInput: value => value === null ? '' : String(value),
    parseAccountSchedulingForm: () => ({ valid: true, values: { concurrencyLimit: null, weight: 50 } }),
    accountModelAccessError: () => null,
    apiKeyAccountError: () => null,
    emptyApiKeyAccountForm: () => ({ apiKey: '', base_url: '', transport: 'http' }),
    useRequestState: () => ({ start: () => 1, isCurrent: () => true, fail() {}, finish() {}, invalidate() {}, loading: Vue.ref(false) }),
  }
  deps.useAsyncAction = () => ({
    loading: Vue.ref(false),
    run: async (fn) => {
      try {
        return await fn()
      }
      catch (error) { deps.toast.error(error.message) }
    },
  })
  const create = loadComposable('../../plugins/accounts/ui/page/composables/useAccountEditor.ts', 'useAccountEditor', deps)
  const scope = Vue.effectScope()
  const editor = scope.run(() => create({ accounts: Vue.ref([row]), reloadAccounts: async () => {}, reloadGroups: async () => {} }))
  editor.open(row)
  return { editor, row, calls, messages, savedProtection: () => saved, stop: () => scope.stop() }
}

function batchFixture() {
  const calls = []
  const rows = [
    { id: 'one', enabled: true, weight: 50, groups: [], concurrencyLimit: null, quotaProtection: { enabled: true, usedPercentLimit: 80 } },
    { id: 'two', enabled: true, weight: 50, groups: [], concurrencyLimit: null, quotaProtection: { enabled: false, usedPercentLimit: 90 } },
  ]
  const deps = {
    ...Vue,
    batchUpdateAccounts: async input => calls.push(input),
    toast: { success() {}, warning: assert.fail },
    useAsyncAction: () => ({ loading: Vue.ref(false), run: async fn => fn() }),
    accountModelAccessError: () => null,
    concurrencyLimitInput: value => value === null ? '' : String(value),
    parseAccountSchedulingForm: () => ({ valid: true, values: { concurrencyLimit: null, weight: 50 } }),
  }
  const create = loadComposable('../../plugins/accounts/ui/page/composables/useAccountBatchEditor.ts', 'useAccountBatchEditor', deps)
  const scope = Vue.effectScope()
  const editor = scope.run(() => create({ accounts: Vue.ref(rows), selectedIds: Vue.ref(new Set(rows.map(row => row.id))), reloadAccounts: async () => {}, reloadGroups: async () => {} }))
  editor.open()
  return { editor, calls, rows, stop: () => scope.stop() }
}

test('quota threshold accepts only integer percentages 1 through 99', () => {
  assert.deepEqual(QUOTA_PROTECTION_PRESETS, [85, 90, 95])
  for (const input of ['', ' ', '0', '100', '-1', '+90', '90.5', '90.0', 'NaN', 'Infinity', '1e1', '0x5a'])
    assert.equal(parseQuotaProtection(true, input), null, input)
  assert.deepEqual(parseQuotaProtection(false, '90'), { enabled: false, usedPercentLimit: 90 })
  assert.deepEqual(parseQuotaProtection(true, '1'), { enabled: true, usedPercentLimit: 1 })
  assert.deepEqual(parseQuotaProtection(true, '99'), { enabled: true, usedPercentLimit: 99 })
  assert.equal(supportsQuotaProtection(undefined), false)
  assert.equal(supportsQuotaProtection({ enabled: false, usedPercentLimit: 0 }), false)
  assert.equal(supportsQuotaProtection({ enabled: 'true', usedPercentLimit: 90 }), false)
  assert.equal(supportsQuotaProtection({ enabled: true, usedPercentLimit: 90.5 }), false)
  assert.equal(supportsQuotaProtection({ enabled: true, usedPercentLimit: '90' }), false)
})

test('editing quota protection uses one atomic account update and confirms readback before success', async () => {
  const f = fixture()
  try {
    assert.equal(f.editor.quotaProtectionLimit.value, '90')
    assert.equal(f.editor.quotaProtectionEnabled.value, false)
    f.editor.name.value = '新名称'
    f.editor.quotaProtectionEnabled.value = true
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][0], 'update')
    assert.equal(f.calls[0][1].name, '新名称')
    assert.deepEqual(f.calls[0][1].quotaProtection, { enabled: true, usedPercentLimit: 90 })
    assert.equal(f.row.enabled, true)
    assert.equal(f.messages[0][0], 'success')
    assert.equal(f.editor.showEditModal.value, false)
  }
  finally { f.stop() }
})

test('legacy gateway disables quota protection and never claims a protection save', async () => {
  const f = fixture({ protection: null })
  try {
    assert.equal(f.editor.quotaProtectionSupported.value, false)
    f.editor.quotaProtectionEnabled.value = true
    await f.editor.save()
    assert.equal(f.calls.length, 0)
    assert.match(f.messages[0][1], /不支持/)
    assert.equal(f.editor.showEditModal.value, true)
  }
  finally { f.stop() }
})

test('a downgraded gateway that ignores the new field cannot produce a success toast', async () => {
  const f = fixture({ ignore: true })
  try {
    f.editor.quotaProtectionEnabled.value = true
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.messages.some(([kind]) => kind === 'success'), false)
    assert.match(f.messages[0][1], /未确认养号设置已保存/)
    assert.equal(f.editor.showEditModal.value, true)
  }
  finally { f.stop() }
})

test('invalid threshold is rejected before any write and unchanged edits stay quiet', async () => {
  const f = fixture()
  try {
    f.editor.quotaProtectionEnabled.value = true
    f.editor.quotaProtectionLimit.value = '90.5'
    await f.editor.save()
    assert.equal(f.calls.length, 0)
    assert.match(f.messages[0][1], /1 至 99/)
    f.editor.quotaProtectionLimit.value = '90'
    f.editor.quotaProtectionEnabled.value = false
    await f.editor.save()
    assert.equal(f.calls.length, 0)
    assert.equal(f.messages.length, 1)
    assert.equal(f.editor.showEditModal.value, false)
  }
  finally { f.stop() }
})

test('disabling protection preserves its saved threshold despite invalid hidden input', async () => {
  const f = fixture({ protection: { enabled: true, usedPercentLimit: 85 } })
  try {
    f.editor.quotaProtectionLimit.value = 'invalid'
    f.editor.quotaProtectionEnabled.value = false
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.deepEqual(f.calls[0][1].quotaProtection, { enabled: false, usedPercentLimit: 85 })
    assert.deepEqual(f.savedProtection(), { enabled: false, usedPercentLimit: 85 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('disabled protection ignores draft threshold changes and leaves saved policy untouched', async () => {
  const f = fixture({ protection: { enabled: false, usedPercentLimit: 85 } })
  try {
    f.editor.quotaProtectionEnabled.value = true
    f.editor.quotaProtectionLimit.value = '70'
    f.editor.quotaProtectionEnabled.value = false
    f.editor.name.value = '仅改名称'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][1].quotaProtection, undefined)
    assert.deepEqual(f.savedProtection(), { enabled: false, usedPercentLimit: 85 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('unrelated account settings do not rewrite enabled quota protection', async () => {
  const f = fixture({ protection: { enabled: true, usedPercentLimit: 80 } })
  try {
    f.editor.name.value = '仅改名称'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][1].quotaProtection, undefined)
    assert.deepEqual(f.savedProtection(), { enabled: true, usedPercentLimit: 80 })
  }
  finally { f.stop() }
})

test('group-only edits keep using the narrow batch update without quota protection', async () => {
  const f = fixture()
  try {
    f.editor.selectedGroupIds.value = ['new-group']
    await f.editor.save()
    assert.deepEqual(f.calls, [['batch', { accountIds: ['test-account'], groupIds: ['new-group'] }]])
  }
  finally { f.stop() }
})

test('legacy gateway still saves unrelated account edits without the new field', async () => {
  const f = fixture({ protection: null })
  try {
    f.editor.name.value = '仅改名称'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][1].quotaProtection, undefined)
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('batch edits preserve different per-account protection policies', async () => {
  const f = batchFixture()
  try {
    f.editor.schedulingEnabled.value = false
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0].enabled, false)
    assert.equal(Object.hasOwn(f.calls[0], 'quotaProtection'), false)
    assert.deepEqual(f.rows.map(row => row.quotaProtection), [
      { enabled: true, usedPercentLimit: 80 },
      { enabled: false, usedPercentLimit: 90 },
    ])
  }
  finally { f.stop() }
})

test('API Key connection edits carry protection in the same rotate settings and verify it', async () => {
  const f = fixture({ apiKey: true })
  try {
    await Vue.nextTick()
    f.editor.apiKey.value.apiKey = 'mock-invalid-key'
    f.editor.quotaProtectionEnabled.value = true
    f.editor.quotaProtectionLimit.value = '80'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][0], 'rotate')
    assert.deepEqual(f.calls[0][1].settings.quotaProtection, { enabled: true, usedPercentLimit: 80 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('API Key rotation does not rewrite unchanged quota protection', async () => {
  const f = fixture({ apiKey: true, protection: { enabled: true, usedPercentLimit: 80 } })
  try {
    await Vue.nextTick()
    f.editor.apiKey.value.apiKey = 'mock-invalid-key'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][0], 'rotate')
    assert.equal(f.calls[0][1].settings.quotaProtection, undefined)
    assert.deepEqual(f.savedProtection(), { enabled: true, usedPercentLimit: 80 })
  }
  finally { f.stop() }
})

const splitProtection = { enabled: true, usedPercentLimit: 90, shortTermUsedPercentLimit: 85, weeklyUsedPercentLimit: 95 }

test('OpenAI saves independent window thresholds atomically while retaining fallback', async () => {
  const f = fixture({ provider: 'openai', protection: splitProtection })
  try {
    assert.equal(f.editor.quotaProtectionSupported.value, true)
    assert.equal(f.editor.quotaProtectionShortTermLimit.value, '85')
    assert.equal(f.editor.quotaProtectionWeeklyLimit.value, '95')
    f.editor.name.value = '分周期'
    f.editor.quotaProtectionShortTermLimit.value = '80'
    f.editor.quotaProtectionWeeklyLimit.value = '93'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.deepEqual(f.calls[0][1].quotaProtection, { ...splitProtection, shortTermUsedPercentLimit: 80, weeklyUsedPercentLimit: 93 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('OpenAI legacy thresholds map to both windows but cannot be edited on legacy gateways', async () => {
  for (const protection of [
    { enabled: true, usedPercentLimit: 85 },
    { ...splitProtection, shortTermUsedPercentLimit: undefined },
    { ...splitProtection, weeklyUsedPercentLimit: 90.5 },
  ]) {
    const f = fixture({ provider: 'openai', protection })
    try {
      assert.equal(f.editor.quotaProtectionSupported.value, false)
      if (!Object.hasOwn(protection, 'shortTermUsedPercentLimit')) {
        assert.equal(f.editor.quotaProtectionShortTermLimit.value, '85')
        assert.equal(f.editor.quotaProtectionWeeklyLimit.value, '85')
      }
      f.editor.quotaProtectionEnabled.value = false
      await f.editor.save()
      assert.equal(f.calls.length, 0)
      assert.match(f.messages[0][1], /不支持/)
    }
    finally { f.stop() }
  }
})

test('both split thresholds reject invalid inputs before writes', async () => {
  for (const field of ['quotaProtectionShortTermLimit', 'quotaProtectionWeeklyLimit']) {
    const f = fixture({ provider: 'openai', protection: splitProtection })
    try {
      f.editor[field].value = '90.5'
      await f.editor.save()
      assert.equal(f.calls.length, 0)
      assert.match(f.messages[0][1], /1 至 99/)
    }
    finally { f.stop() }
  }
})

test('disabling split protection preserves all saved thresholds and ignores hidden invalid drafts', async () => {
  const f = fixture({ provider: 'openai', protection: splitProtection })
  try {
    f.editor.quotaProtectionShortTermLimit.value = 'bad'
    f.editor.quotaProtectionWeeklyLimit.value = '100'
    f.editor.quotaProtectionLimit.value = 'bad'
    f.editor.quotaProtectionEnabled.value = false
    await f.editor.save()
    assert.deepEqual(f.calls[0][1].quotaProtection, { ...splitProtection, enabled: false })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('unchanged split protection stays out of unrelated updates and disabled drafts', async () => {
  for (const enabled of [true, false]) {
    const f = fixture({ provider: 'openai', protection: { ...splitProtection, enabled } })
    try {
      f.editor.name.value = '仅改名称'
      if (!enabled) {
        f.editor.quotaProtectionShortTermLimit.value = 'bad'
        f.editor.quotaProtectionWeeklyLimit.value = '50'
      }
      await f.editor.save()
      assert.equal(f.calls[0][1].quotaProtection, undefined)
      assert.equal(f.messages[0][0], 'success')
    }
    finally { f.stop() }
  }
})

test('split save fails readback when any of all four fields differs or disappears', async () => {
  for (const field of ['enabled', 'usedPercentLimit', 'shortTermUsedPercentLimit', 'weeklyUsedPercentLimit']) {
    const f = fixture({ provider: 'openai', protection: splitProtection, readback: saved => ({ ...saved, [field]: undefined }) })
    try {
      f.editor.quotaProtectionWeeklyLimit.value = '94'
      await f.editor.save()
      assert.equal(f.calls.length, 1)
      assert.equal(f.messages.some(([kind]) => kind === 'success'), false)
      assert.match(f.messages[0][1], /未确认/)
      assert.equal(f.editor.showEditModal.value, true)
    }
    finally { f.stop() }
  }
})

test('API key connection and split policy save share a single rotate request', async () => {
  const f = fixture({ provider: 'openai', protection: splitProtection, apiKey: true })
  try {
    await Vue.nextTick()
    f.editor.apiKey.value.apiKey = 'fixture-key'
    f.editor.quotaProtectionWeeklyLimit.value = '92'
    await f.editor.save()
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0][0], 'rotate')
    assert.deepEqual(f.calls[0][1].settings.quotaProtection, { ...splitProtection, weeklyUsedPercentLimit: 92 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

test('xAI billing threshold changes retain split fields returned by a new gateway', async () => {
  const f = fixture({ protection: splitProtection })
  try {
    f.editor.quotaProtectionLimit.value = '88'
    await f.editor.save()
    assert.deepEqual(f.calls[0][1].quotaProtection, { ...splitProtection, usedPercentLimit: 88 })
    assert.equal(f.messages[0][0], 'success')
  }
  finally { f.stop() }
})

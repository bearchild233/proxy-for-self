/* eslint-disable no-new-func -- 执行实际 TS/SFC 源码并注入受控依赖，验证后台刷新逻辑 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库现有逻辑测试共用内置 runner
import test from 'node:test'
import * as Vue from 'vue'
import { refreshAccountPage } from '../../plugins/accounts/ui/page/utils/refreshAccountPage.ts'

function load(path, name, deps) {
  const code = stripTypeScriptTypes(readFileSync(new URL(path, import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import .*$/gm, '')
    .replace(/^export /gm, '')
  const entries = Object.entries(deps).filter(([key]) => /^[a-z_$][\w$]*$/i.test(key) && key !== 'default')
  return new Function(...entries.map(([key]) => key), `${code}; return ${name}`)(...entries.map(([, value]) => value))
}

function fixture() {
  const lifecycle = { ...Vue, onMounted() {}, useIntervalFn() {}, watchDebounced() {} }
  const useRequestState = load('../src/composables/useRequestState.ts', 'useRequestState', { ...lifecycle, ApiError: Error, errorMessage: String })
  const usePagedQuery = load('../src/composables/usePagedQuery.ts', 'usePagedQuery', { ...lifecycle, useRequestState, clamp: (v, lo, hi) => Math.min(hi, Math.max(lo, v)) })
  const row = { id: 'a', name: 'A', enabled: true, provider: 'openai', authenticationKind: 'oauth', quota: 10 }
  let next = { ...row }
  let fail = true
  let pending
  let resume
  const useAccountsQuery = load('../../plugins/accounts/ui/page/composables/useAccountsQuery.ts', 'useAccountsQuery', {
    ...lifecycle,
    usePagedQuery,
    usePluginVisibility: () => Vue.ref('visible'),
    refreshAccountPage,
    refreshAccountQuota: async () => {
      await pending
      if (fail)
        throw new Error('upstream')
    },
    getAccounts: async ({ page, pageSize }) => ({ items: [next], summary: {}, page: { page, pageSize, total: 30, totalPages: 2 } }),
    getAccountPersonalInfo: async () => ({}),
    accountNeedsAttention: () => true,
    formatTime: () => '2026-10-08 12:00:00',
    toast: {
      warning() { assert.fail('silent refresh must not toast') },
      success() { assert.fail('silent refresh must not toast') },
    },
  })
  const usePageSelection = load('../src/composables/usePageSelection.ts', 'usePageSelection', lifecycle)
  const useAccountsTable = load('../../plugins/accounts/ui/page/composables/useAccountsTable.ts', 'useAccountsTable', { ...lifecycle, usePageSelection })
  const scope = Vue.effectScope()
  const q = scope.run(useAccountsQuery)
  q.accounts.value = [row]
  const table = scope.run(() => useAccountsTable(q.accounts))
  return {
    q,
    table,
    stop: () => scope.stop(),
    succeed() {
      fail = false
      next = { ...row, quota: 20 }
    },
    pause() {
      pending = new Promise((r) => {
        resume = r
      })
    },
    resume() { resume() },
  }
}

test('background refresh keeps rows, page, selection, expansion and previous status while pending', async () => {
  const f = fixture()
  try {
    f.q.page.value = 2
    await Vue.nextTick()
    f.table.toggleExpanded('a')
    f.table.toggleSelection('a')
    await f.q.refreshAccounts(true)
    const message = f.q.refreshMessage.value
    assert.match(message, /未同步/)
    const rows = f.q.accounts.value
    f.pause()
    f.succeed()
    const refreshing = f.q.refreshAccounts(true)
    await Vue.nextTick()
    assert.equal(f.q.loading.value, false)
    assert.equal(f.q.accounts.value, rows)
    assert.equal(f.q.refreshMessage.value, message)
    f.resume()
    await refreshing
    assert.equal(f.q.accounts.value[0].quota, 20)
    assert.equal(f.q.page.value, 2)
    assert.deepEqual(f.table.expandedRowKeys.value, ['a'])
    assert.deepEqual(f.table.selectedRowKeys.value, ['a'])
    assert.equal(f.q.refreshMessage.value, '')
  }
  finally { f.stop() }
})

test('repeated failed synchronization does not clear then reinsert the same status', async () => {
  const f = fixture()
  try {
    await f.q.refreshAccounts(true)
    const messages = []
    const unwatch = Vue.watch(f.q.refreshMessage, value => messages.push(value), { flush: 'sync' })
    await f.q.refreshAccounts(true)
    unwatch()
    assert.deepEqual(messages, [])
  }
  finally { f.stop() }
})

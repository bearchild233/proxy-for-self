/* eslint-disable no-new-func -- 执行实际 composable，注入受控 API 和时钟 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与现有测试使用相同 runner
import test from 'node:test'
import * as Vue from 'vue'

function load(path, name, deps) {
  const code = stripTypeScriptTypes(readFileSync(new URL(path, import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import\s[\s\S]*?from ['"][^'"]+['"];?\r?\n/gm, '')
    .replace(/^export /gm, '')
  const entries = Object.entries(deps).filter(([key]) => /^[a-z_$][\w$]*$/i.test(key) && key !== 'default')
  return new Function(...entries.map(([key]) => key), `${code}
return ${name}`)(...entries.map(([, value]) => value))
}

const lifecycle = { ...Vue, onMounted() {}, watchDebounced() {} }
const toast = {
  success() {},
  warning() {},
  error() { assert.fail('background errors must be silent') },
}
const useRequestState = load('../src/composables/useRequestState.ts', 'useRequestState', { ...lifecycle, ApiError: Error, errorMessage: String })
const usePagedQuery = load('../src/composables/usePagedQuery.ts', 'usePagedQuery', { ...lifecycle, useRequestState, clamp: (v, lo, hi) => Math.min(hi, Math.max(lo, v)) })
const useAsyncAction = load('../src/composables/useAsyncAction.ts', 'useAsyncAction', { ...lifecycle, ApiError: Error, toast, errorMessage: String, withMinimumDuration: fn => fn() })
const useIdSet = load('../src/composables/useIdSet.ts', 'useIdSet', lifecycle)
function deferred() {
  let resolve
  const promise = new Promise(r => resolve = r)
  return { promise, resolve }
}

test('visible polling waits 30s initially, refreshes on return, stops while hidden/disposed', async () => {
  const tasks = new Map()
  let id = 0
  let calls = 0
  const polling = load('../src/composables/useVisiblePolling.ts', 'useVisiblePolling', {
    ...lifecycle,
    setTimeout(fn, delay) {
      tasks.set(++id, { fn, delay })
      return id
    },
    clearTimeout(key) { tasks.delete(key) },
  })
  const visibility = Vue.ref('visible')
  const interval = Vue.ref(30_000)
  const scope = Vue.effectScope()
  scope.run(() => polling(async () => {
    calls++
  }, interval, visibility))
  assert.equal([...tasks.values()][0].delay, 30_000)
  visibility.value = 'hidden'
  await Vue.nextTick()
  assert.equal(tasks.size, 0)
  visibility.value = 'visible'
  await Vue.nextTick()
  assert.equal([...tasks.values()][0].delay, 0)
  const current = [...tasks.values()][0]
  tasks.clear()
  await current.fn()
  assert.equal(calls, 1)
  assert.equal([...tasks.values()][0].delay, 30_000)
  interval.value = 0
  await Vue.nextTick()
  assert.equal(tasks.size, 0)
  interval.value = 60_000
  await Vue.nextTick()
  scope.stop()
  assert.equal(tasks.size, 0)
})

test('polling serializes slow requests and retries after failures without unhandled rejection', async () => {
  const tasks = new Map()
  let id = 0
  const work = deferred()
  let calls = 0
  const visibility = Vue.ref('visible')
  const polling = load('../src/composables/useVisiblePolling.ts', 'useVisiblePolling', {
    ...lifecycle,
    setTimeout(fn, delay) {
      tasks.set(++id, { fn, delay })
      return id
    },
    clearTimeout(key) { tasks.delete(key) },
  })
  const scope = Vue.effectScope()
  scope.run(() => polling(async () => {
    calls++
    await work.promise
    throw new Error('offline')
  }, Vue.ref(30_000), visibility))
  const first = [...tasks.values()][0]
  tasks.clear()
  const pending = first.fn()
  visibility.value = 'hidden'
  await Vue.nextTick()
  visibility.value = 'visible'
  await Vue.nextTick()
  assert.equal(tasks.size, 0)
  assert.equal(calls, 1)
  work.resolve()
  await pending
  assert.equal(tasks.size, 1)
  scope.stop()
})

test('key list background refresh keeps page, sort and rows while pending, updates budgets silently', async () => {
  let poll
  const requests = []
  let block
  let used = 1
  const useKeys = load('../../plugins/keys/ui/page/composables/useApiKeysQuery.ts', 'useApiKeysQuery', {
    ...lifecycle,
    useRequestState,
    formatDateTime: String,
    usePluginPolling(fn) { poll = fn },
    async getApiKeys(params, options) {
      requests.push({ ...params, options })
      await block?.promise
      return { items: [{ id: params.cursor ? 'k2' : 'k1', createdAt: '', dailyUsedUsd: used }], total: 40, nextCursor: params.cursor ? undefined : 'page2' }
    },
  })
  const scope = Vue.effectScope()
  const q = scope.run(useKeys)
  await q.loadApiKeys()
  q.handlePageChange(2)
  await new Promise(r => setImmediate(r))
  assert.equal(q.apiKeyPagination.value.currentPage, 2)
  const rows = q.apiKeys.value
  used = 7
  block = deferred()
  const job = poll()
  assert.equal(q.loading.value, false)
  assert.equal(q.apiKeys.value, rows)
  const count = requests.length
  await poll()
  assert.equal(requests.length, count)
  block.resolve()
  await job
  assert.equal(q.apiKeyPagination.value.currentPage, 2)
  assert.equal(q.apiKeys.value[0].dailyUsedUsd, 7)
  assert.equal(requests.at(-1).options.silent, true)
  scope.stop()
})

test('usage polling updates analytics to now, keeps later-page snapshot and does not reset filters', async () => {
  let poll
  let time = 't1'
  const tables = []
  const summaries = []
  const useUsage = load('../../plugins/usage-analytics/ui/usage/composables/useUsageRecordsTable.ts', 'useUsageRecordsTable', {
    ...lifecycle,
    withMinimumDuration: fn => fn(),
    usePluginPolling(fn) { poll = fn },
    async getUsageRecords(params, options) {
      tables.push({ ...params, options })
      return { items: [{ id: 'row' }], currentPage: params.currentPage, pageSize: params.pageSize, total: 40 }
    },
    async getUsageRecordSummary(params) {
      summaries.push(params)
      return { totalRequests: time }
    },
    async getUsageRecordInsightsOverview() { return {} },
    async getUsageRecordInsightsDiagnostics() { return {} },
  })
  const scope = Vue.effectScope()
  const active = Vue.ref(true)
  const q = scope.run(() => useUsage({ active, timeRangeParams: Vue.ref({ startTime: 'start', endTime: 't1' }), latestTimeRangeParams: () => ({ startTime: 'start', endTime: time }) }))
  await q.loadUsageRecords()
  time = 't2'
  await poll()
  assert.equal(tables.at(-1).endTime, 't2')
  assert.equal(summaries.at(-1).endTime, 't2')
  q.handlePageChange(2)
  await new Promise(r => setImmediate(r))
  time = 't3'
  await poll()
  assert.equal(q.currentPage.value, 2)
  assert.equal(tables.at(-1).endTime, 't2')
  assert.equal(summaries.at(-1).endTime, 't3')
  assert.equal(tables.at(-1).options.silent, true)
  assert.equal(q.loading.value, false)
  assert.equal(q.analyticsLoading.value, false)
  active.value = false
  await Vue.nextTick()
  const count = tables.length
  time = 't4'
  await poll()
  assert.equal(tables.length, count)
  assert.equal(q.summary.value.totalRequests, 't4')
  scope.stop()
})

test('group polling updates metrics without overwriting open editor or selection', async () => {
  let poll
  let cost = 1
  const useGroups = load('../../plugins/groups/ui/page/composables/useAccountGroups.ts', 'useAccountGroups', {
    ...lifecycle,
    usePagedQuery,
    useAsyncAction,
    useIdSet,
    toast,
    errorMessage: String,
    normalizeRgbaHexColor: v => v,
    formatDateTime: String,
    DEFAULT_ACCOUNT_GROUP_COLOR: '#fff',
    usePluginPolling(fn) { poll = fn },
    async getAccountGroups({ page, pageSize }, options) {
      assert.equal(options.silent, true)
      return { items: [{ id: 'g', name: 'server', cost }], page: { page, pageSize, total: 25, totalPages: 2 } }
    },
  })
  const scope = Vue.effectScope()
  const q = scope.run(useGroups)
  await poll()
  q.openEdit(q.groups.value[0])
  q.form.value.name = 'unsaved'
  q.selectedIds.value.add('g')
  cost = 9
  await poll()
  assert.equal(q.groups.value[0].cost, 9)
  assert.equal(q.form.value.name, 'unsaved')
  assert.equal(q.showFormModal.value, true)
  assert.equal(q.selectedIds.value.has('g'), true)
  scope.stop()
})

test('backup polling discovers scheduled jobs when idle and speeds up active jobs', async () => {
  let poll
  let interval
  let status = 'completed'
  const useBackups = load('../../plugins/backup/ui/composables/useBackupRecords.ts', 'useBackupRecords', {
    ...lifecycle,
    usePagedQuery,
    toast,
    ApiError: Error,
    errorMessage: String,
    usePluginPolling(fn, ms) {
      poll = fn
      interval = ms
    },
    async getBackupRecords({ page, pageSize }) { return { items: [{ id: 'b', status }], page: { page, pageSize, total: 1, totalPages: 1 } } },
  })
  const scope = Vue.effectScope()
  const q = scope.run(useBackups)
  q.startPolling()
  assert.equal(interval.value, 30_000)
  status = 'uploading'
  await poll()
  assert.equal(interval.value, 2000)
  status = 'completed'
  await poll()
  assert.equal(interval.value, 30_000)
  q.stopPolling()
  assert.equal(interval.value, 0)
  scope.stop()
})

test('backup settings protect drafts typed during an in-flight refresh', async () => {
  let poll
  let block
  const settings = { storageRevision: 1, endpoint: 'saved', retentionDays: 7, retentionCount: 7 }
  const useSettings = load('../../plugins/backup/ui/composables/useBackupSettings.ts', 'useBackupSettings', {
    ...lifecycle,
    toast,
    errorMessage: String,
    usePluginPolling(fn) { poll = fn },
    async getBackupSettings() {
      await block?.promise
      return settings
    },
  })
  const scope = Vue.effectScope()
  const q = scope.run(useSettings)
  await q.load()
  block = deferred()
  const job = poll()
  q.storage.endpoint = 'unsaved'
  block.resolve()
  await job
  assert.equal(q.storage.endpoint, 'unsaved')
  assert.equal(q.loading.value, false)
  scope.stop()
})

test('gateway settings synchronize server values but never overwrite a draft typed during refresh', async () => {
  let poll
  let block
  let limit = 40
  const useSettings = load('../../plugins/gateway-settings/ui/page/composables/useSettingsForm.ts', 'useSettingsForm', {
    ...lifecycle,
    useAsyncAction,
    toast,
    ApiError: Error,
    errorMessage: String,
    normalizeRequestLocation: value => value,
    requestLocationError: () => '',
    usePluginPolling(fn) { poll = fn },
    async updateRequestLocation() { assert.fail('reading settings must not write back') },
    async getSettings() {
      await block?.promise
      return { requestLocation: { timezone: 'America/Los_Angeles' }, requestLocationEnabled: true, inferenceLimits: { maxRequests: limit, maxBodyBytes: 50 * 1024 * 1024, maxInFlightBodyBytes: 512 * 1024 * 1024 }, modelMappings: {} }
    },
  })
  const scope = Vue.effectScope()
  const q = scope.run(useSettings)
  await q.loadSettings()
  limit = 35
  await poll()
  assert.equal(q.form.gatewayMaxRequests, 35)
  assert.equal(q.hasChanges.value, false)
  block = deferred()
  const job = poll()
  q.form.gatewayMaxRequests = 25
  block.resolve()
  await job
  assert.equal(q.form.gatewayMaxRequests, 25)
  assert.equal(q.hasChanges.value, true)
  assert.equal(q.loading.value, false)
  scope.stop()
})

/* eslint-disable no-new-func -- 测试执行实际源码并注入受控依赖 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { preloadPlan, runPreloads } from '../src/plugins/preloadPlan.ts'
import { SharedResources } from '../src/plugins/sharedResources.ts'

function plugin(id, routes, lazyLoad = false) {
  return {
    id,
    contentVersion: 'v1',
    lazyLoad,
    pages: routes.map((route, i) => ({
      id: String(i),
      route,
      runtime: 'vue-component',
    })),
  }
}

test('preload current route then core pages; lazy packages excluded and component subpages deduplicated', () => {
  const result = preloadPlan(
    [
      plugin('popup', [undefined]),
      plugin('settings', ['/settings', '/settings/access']),
      plugin('accounts', ['/accounts']),
      plugin('dashboard', ['/']),
      plugin('lazy', ['/keys'], true),
    ],
    '/settings/access',
  )
  assert.deepEqual(
    result.map(p => p.key),
    ['settings/v1/0', 'dashboard/v1/0', 'accounts/v1/0', 'popup/v1/0'],
  )
})
test('preloads are bounded to three concurrent requests and one failure does not block queue', async () => {
  let active = 0
  let peak = 0
  const seen = []
  await runPreloads(
    [1, 2, 3, 4, 5, 6],
    async (value) => {
      active++
      peak = Math.max(peak, active)
      seen.push(value)
      await new Promise(resolve => setImmediate(resolve))
      active--
      if (value === 2)
        throw new Error('failed package')
    },
    () => true,
  )
  assert.equal(peak, 3)
  assert.deepEqual(seen, [1, 2, 3, 4, 5, 6])
})
test('identity invalidation stops queued downloads', async () => {
  let valid = true
  const seen = []
  await runPreloads(
    [1, 2, 3, 4],
    async (value) => {
      seen.push(value)
      valid = false
    },
    () => valid,
  )
  assert.deepEqual(seen, [1])
})

function resources() {
  let now = 0
  let nextTimer = 0
  let sequence = 0
  const timers = new Map()
  const requests = []
  const document = { hidden: false }
  const request = async (config) => {
    requests.push(config)
    return config.url.includes('/page/')
      ? { html: 'module source', lease: 'prefetched' }
      : { lease: config.data.lease || `new-${++sequence}` }
  }
  const timer = (callback, delay, ...args) => {
    const id = ++nextTimer
    if (!delay)
      queueMicrotask(() => callback(...args))
    else timers.set(id, () => callback(...args))
    return id
  }
  const source = stripTypeScriptTypes(
    readFileSync(
      new URL('../src/plugins/pageResources.ts', import.meta.url),
      'utf8',
    ),
    { mode: 'strip' },
  )
    .replace(/^import .*$/gm, '')
    .replace(/^export /gm, '')
  const api = new Function(
    'request',
    'prepareComponent',
    'retainComponentResources',
    'SharedResources',
    'preloadPlan',
    'runPreloads',
    'document',
    'location',
    'setTimeout',
    'clearTimeout',
    'Date',
    `${source};return {syncPageResources,loadPluginPage,resetPageResources};`,
  )(
    request,
    async () => {},
    () => {},
    SharedResources,
    preloadPlan,
    runPreloads,
    document,
    { pathname: '/' },
    timer,
    id => timers.delete(id),
    { now: () => now },
  )
  return {
    ...api,
    requests,
    document,
    time: (value) => {
      now = value
    },
    renew: async () => {
      const [id, callback] = timers.entries().next().value
      timers.delete(id)
      await callback()
    },
  }
}
const settle = () => new Promise(resolve => setImmediate(resolve))
test('visible prefetch renews and is adopted after 90 seconds without click-time lease creation', async () => {
  const r = resources()
  r.syncPageResources([plugin('dashboard', ['/'])])
  await settle()
  r.time(45000)
  await r.renew()
  r.time(91000)
  const page = await r.loadPluginPage('dashboard', 'v1', '0')
  assert.equal(page.lease, 'prefetched')
  assert.equal(r.requests.filter(q => q.data?.lease === null).length, 0)
  page.release()
  r.resetPageResources()
})
test('expired prefetch is never adopted even when background timers were suspended', async () => {
  const r = resources()
  r.syncPageResources([plugin('dashboard', ['/'])])
  await settle()
  r.time(130000)
  const page = await r.loadPluginPage('dashboard', 'v1', '0')
  assert.equal(page.lease, 'new-1')
  page.release()
  r.resetPageResources()
})
test('hidden tab stops renewing unused package leases', async () => {
  const r = resources()
  r.syncPageResources([plugin('dashboard', ['/'])])
  await settle()
  r.document.hidden = true
  await r.renew()
  assert.equal(r.requests.at(-1).data.release, true)
  r.resetPageResources()
})

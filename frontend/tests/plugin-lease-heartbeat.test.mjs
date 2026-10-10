import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 使用仓库统一逻辑测试入口
import test from 'node:test'
import { maintainPluginLease } from '../src/plugins/leaseHeartbeat.ts'

test('default browser timers retain their required Window receiver', () => {
  const document = new EventTarget()
  document.hidden = false
  const window = new EventTarget()
  const tasks = new Map()
  window.setTimeout = function (callback, delay) {
    assert.equal(this, window, 'native timer receiver')
    tasks.set(1, { callback, delay })
    return 1
  }
  window.clearTimeout = function (id) {
    assert.equal(this, window, 'native timer receiver')
    tasks.delete(id)
  }
  const originals = new Map(['document', 'window', 'setTimeout', 'clearTimeout'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]))
  try {
    for (const [key, value] of Object.entries({ document, window, setTimeout: window.setTimeout, clearTimeout: window.clearTimeout }))
      Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
    const stop = maintainPluginLease({ renew: async () => {}, unavailable: async () => {} })
    assert.equal(tasks.get(1).delay, 30_000)
    stop()
    assert.equal(tasks.size, 0)
  }
  finally {
    for (const [key, descriptor] of originals) {
      if (descriptor)
        Object.defineProperty(globalThis, key, descriptor)
      else
        delete globalThis[key]
    }
  }
})

function fixture(renew, unavailable = async () => {}) {
  const tasks = new Map()
  const document = new EventTarget()
  document.hidden = false
  const window = new EventTarget()
  let id = 0
  const stop = maintainPluginLease({ renew, unavailable }, {
    document,
    window,
    setTimeout(fn, delay) {
      tasks.set(++id, { fn, delay })
      return id
    },
    clearTimeout(key) { tasks.delete(key) },
  })
  return {
    tasks,
    document,
    window,
    stop,
    async tick() {
      const task = [...tasks.values()][0]
      tasks.clear()
      await task.fn()
    },
  }
}

test('network errors retry quietly without invalidating the mounted page', async () => {
  let calls = 0
  let invalidations = 0
  const f = fixture(async () => {
    if (++calls === 1)
      throw new Error('offline')
  }, async () => { invalidations++ })
  assert.equal([...f.tasks.values()][0].delay, 30_000)
  await f.tick()
  assert.equal(invalidations, 0)
  assert.equal([...f.tasks.values()][0].delay, 5000)
  await f.tick()
  assert.equal([...f.tasks.values()][0].delay, 30_000)
  f.stop()
})

test('wake and online renew immediately; disposal removes event handlers and timers', async () => {
  let calls = 0
  const f = fixture(async () => {
    calls++
  })
  f.document.hidden = true
  f.document.dispatchEvent(new Event('visibilitychange'))
  assert.equal(calls, 0)
  f.document.hidden = false
  f.document.dispatchEvent(new Event('visibilitychange'))
  assert.equal(calls, 1)
  await new Promise(resolve => setImmediate(resolve))
  f.window.dispatchEvent(new Event('online'))
  assert.equal(calls, 2)
  await new Promise(resolve => setImmediate(resolve))
  f.stop()
  f.window.dispatchEvent(new Event('pageshow'))
  assert.equal(calls, 2)
  assert.equal(f.tasks.size, 0)
})

test('slow heartbeats do not overlap or invalidate a disposed page', async () => {
  let resolve
  let calls = 0
  let invalidations = 0
  const f = fixture(async () => {
    calls++
    await new Promise(r => resolve = r)
    throw Object.assign(new Error('version missing'), { status: 400 })
  }, async () => { invalidations++ })
  const pending = f.tick()
  f.window.dispatchEvent(new Event('online'))
  assert.equal(calls, 1)
  f.stop()
  resolve()
  await pending
  assert.equal(invalidations, 0)
  assert.equal(f.tasks.size, 0)
})

test('an explicit missing-version response invokes automatic recovery', async () => {
  let recovered = 0
  const f = fixture(async () => {
    throw Object.assign(new Error('version missing'), { status: 400 })
  }, async () => { recovered++ })
  await f.tick()
  assert.equal(recovered, 1)
  f.stop()
})

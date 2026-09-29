import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 使用内置 runner 验证刷新失败与并发边界
import test from 'node:test'
import { refreshAccountPage } from '../src/views/accounts/utils/refreshAccountPage.ts'

function account(id, overrides = {}) {
  return { id, name: id, enabled: true, provider: 'openai', authenticationKind: 'oauth', ...overrides }
}

test('Refresh reaches every eligible account with at most two requests, preserving partial failures', async () => {
  let active = 0
  let maximum = 0
  const called = []
  const result = await refreshAccountPage([
    account('a'),
    account('b'),
    account('c'),
    account('disabled', { enabled: false }),
    account('api-key', { authenticationKind: 'api_key' }),
  ], async (id) => {
    active += 1
    maximum = Math.max(maximum, active)
    called.push(id)
    await new Promise(resolve => setTimeout(resolve, 1))
    active -= 1
    if (id === 'b')
      throw new Error('Upstream unavailable')
  }, new AbortController().signal)
  assert.equal(maximum, 2)
  assert.deepEqual(called.sort(), ['a', 'b', 'c'])
  assert.deepEqual(result, { succeeded: 2, failed: ['b'], skipped: 2 })
})

test('Leaving the page stops further upstream refresh requests', async () => {
  const controller = new AbortController()
  const called = []
  await refreshAccountPage([account('a'), account('b'), account('c')], async (id) => {
    called.push(id)
    controller.abort()
  }, controller.signal)
  assert.deepEqual(called, ['a'])
})

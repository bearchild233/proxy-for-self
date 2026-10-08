import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { SharedResources } from '../src/plugins/sharedResources.ts'

test('concurrent pages share acquisition and release only after last page closes', async () => {
  let acquisitions = 0
  const releases = []
  const pool = new SharedResources((key, value) =>
    releases.push([key, value.lease]),
  )
  const create = async () => ({ lease: String(++acquisitions) })
  const [first, second] = await Promise.all([
    pool.acquire('settings/v1', create),
    pool.acquire('settings/v1', create),
  ])
  assert.equal(acquisitions, 1)
  assert.equal(first.lease, second.lease)
  first.release()
  first.release()
  assert.deepEqual(releases, [])
  second.release()
  assert.deepEqual(releases, [['settings/v1', '1']])
  const third = await pool.acquire('settings/v1', create)
  assert.equal(third.lease, '2')
  third.release()
})

test('failed acquisition is retriable and independent versions never share', async () => {
  const pool = new SharedResources(() => {})
  await assert.rejects(
    pool.acquire('settings/v1', async () => {
      throw new Error('offline')
    }),
    /offline/,
  )
  const first = await pool.acquire('settings/v1', async () => ({ lease: 'v1' }))
  const second = await pool.acquire('settings/v2', async () => ({
    lease: 'v2',
  }))
  assert.notEqual(first.lease, second.lease)
  first.release()
  second.release()
})

test('session reset separates pending acquisitions without releasing new session lease', async () => {
  const releases = []
  const pool = new SharedResources((key, value) => releases.push(value.lease))
  let finish
  const old = pool.acquire(
    'settings/v1',
    () =>
      new Promise((resolve) => {
        finish = resolve
      }),
  )
  await Promise.resolve()
  pool.clear()
  const current = await pool.acquire('settings/v1', async () => ({
    lease: 'new',
  }))
  finish({ lease: 'old' })
  ;(await old).release()
  assert.deepEqual(releases, ['old'])
  const nextPage = await pool.acquire('settings/v1', async () => {
    throw new Error('should be shared')
  })
  assert.equal(nextPage.lease, 'new')
  current.release()
  assert.deepEqual(releases, ['old'])
  nextPage.release()
  assert.deepEqual(releases, ['old', 'new'])
})

import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- Shared list ordering uses the built-in runner.
import test from 'node:test'
import { applyRowOrder } from '../src/utils/rowOrder.ts'

test('filtered reorder preserves hidden slots and fresh row contents', () => {
  const rows = [{ id: 'a', quota: 7 }, { id: 'hidden' }, { id: 'b', quota: 9 }]
  const sorted = applyRowOrder(rows, ['b', 'a'])
  assert.deepEqual(sorted.map(row => row.id), ['b', 'hidden', 'a'])
  assert.equal(sorted[0], rows[2])
  assert.equal(sorted[1], rows[1])
  assert.equal(applyRowOrder(sorted, ['b', 'a']), sorted)
})

test('changed page or deleted rows cannot introduce missing or duplicate records', () => {
  const rows = [{ id: 'a' }, { id: 'b' }]
  assert.equal(applyRowOrder(rows, ['a', 'a']), rows)
  assert.equal(applyRowOrder(rows, ['b', 'removed']), rows)
})

test('failure rollback uses current objects without overwriting refreshed data', () => {
  const refreshed = [{ id: 'b', quota: 40 }, { id: 'a', quota: 20 }]
  assert.deepEqual(applyRowOrder(refreshed, ['a', 'b']), [refreshed[1], refreshed[0]])
})

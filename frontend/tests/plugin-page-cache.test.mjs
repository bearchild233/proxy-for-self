import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { retainPages } from '../src/plugins/pageCache.ts'

const available = new Map(['a', 'b', 'c', 'd'].map(key => [key, 'v1']))
const page = (key, touched, version = 'v1') => ({ key, touched, version, form: 'draft' })
test('returning preserves page state and DOM order, evicts least recent at limit', () => {
  let pages = [page('a', 1), page('b', 2), page('c', 3)]
  pages = retainPages(pages, page('a', 4), available, 3)
  assert.deepEqual(pages.map(p => p.key), ['a', 'b', 'c'])
  assert.equal(pages[0].form, 'draft')
  pages = retainPages(pages, page('d', 5), available, 3)
  assert.deepEqual(pages.map(p => p.key), ['a', 'c', 'd'])
})
test('update preserves active edits, invalidates old hidden pages; logout removes all', () => {
  const latest = new Map([['a', 'v2'], ['b', 'v1']])
  let pages = retainPages([page('a', 1)], page('a', 2, 'v2'), latest)
  assert.equal(pages[0].version, 'v1')
  pages = retainPages(pages, page('b', 3), latest)
  assert.deepEqual(pages.map(p => p.key), ['b'])
  assert.deepEqual(retainPages(pages, undefined, new Map()), [])
})

test('default retains all available pages beyond eight', () => {
  const all = new Map(Array.from({ length: 12 }, (_, index) => [String(index), 'v1']))
  let cached = []
  for (const [key] of all) cached = retainPages(cached, page(key, Number(key)), all)
  assert.equal(cached.length, 12)
})

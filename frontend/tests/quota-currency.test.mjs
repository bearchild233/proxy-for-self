import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 使用仓库统一逻辑测试入口
import test from 'node:test'
import { quotaAmount, quotaCosts } from '../../plugins/usage-analytics/ui/usage/utils/quota.ts'

test('quota charts compare a single currency without dropping other amounts', () => {
  const item = { costs: [{ currency: 'USD', amount: '1' }, { currency: 'CNY', amount: '7' }], costAmount: null, costCurrency: null }
  assert.equal(quotaAmount(item, 'USD'), 1)
  assert.equal(quotaAmount(item, 'CNY'), 7)
  assert.equal(quotaAmount(item, 'EUR'), 0)
  assert.equal(quotaCosts(item).length, 2)
})

test('legacy single currency is supported, explicit empty or unknown costs stay unknown', () => {
  assert.equal(quotaAmount({ costAmount: '2', costCurrency: 'USD' }, 'USD'), 2)
  assert.equal(quotaAmount({ costs: [], costAmount: '2', costCurrency: 'USD' }, 'USD'), 0)
  assert.deepEqual(quotaCosts({ costAmount: null, costCurrency: null }), [])
})

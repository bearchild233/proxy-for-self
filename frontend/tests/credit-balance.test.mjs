import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 展示边界使用内置 runner
import test from 'node:test'
import { creditBalanceDisplay } from '../../plugins/accounts/ui/page/components/AccountQuotaPanel/credits.ts'

test('credits distinguish numeric, zero, unknown and unlimited balances', () => {
  assert.deepEqual(creditBalanceDisplay({ balance: '62500', usdEquivalent: '2500', unlimited: false }), { balance: '62,500', usd: '$2,500.00' })
  assert.deepEqual(creditBalanceDisplay({ balance: '0', usdEquivalent: '0', unlimited: false }), { balance: '0', usd: '$0.00' })
  assert.deepEqual(creditBalanceDisplay({ balance: '12.5', usdEquivalent: '0.5', unlimited: false }), { balance: '12.5', usd: '$0.50' })
  assert.deepEqual(creditBalanceDisplay({ balance: '62500', usdEquivalent: '2500', unlimited: true }), { balance: '不限', usd: null })
  for (const balance of [null, '', '-1', 'NaN', 'Infinity'])
    assert.deepEqual(creditBalanceDisplay({ balance, usdEquivalent: '2', unlimited: false }), { balance: '未提供', usd: null })
  assert.deepEqual(creditBalanceDisplay(), { balance: '未提供', usd: null })
})

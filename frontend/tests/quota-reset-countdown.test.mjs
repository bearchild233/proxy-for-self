import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 时间边界使用内置 runner
import test from 'node:test'
import { quotaResetCountdown, quotaWindowAbbreviation } from '../../plugins/accounts/ui/page/components/AccountQuotaSummaryCell/presenter.ts'

test('quota reset countdown respects timezone, unknown and expired data', () => {
  const now = Date.parse('2026-10-07T12:00:00Z')
  assert.equal(quotaResetCountdown('2026-10-09T18:00:00Z', now), '2d 6h')
  assert.equal(quotaResetCountdown('2026-10-07T21:23:00+08:00', now), '1h 23m')
  assert.equal(quotaResetCountdown('2026-10-07T12:00:01Z', now), '1m')
  assert.equal(quotaResetCountdown('2026-10-07T12:00:00Z', now), '待同步')
  assert.equal(quotaResetCountdown(undefined, now), '—')
  assert.equal(quotaResetCountdown('invalid', now), '—')
  assert.equal(quotaWindowAbbreviation(18000, '5 小时额度'), '5h')
  assert.equal(quotaWindowAbbreviation(604800, '每周'), 'Week')
})

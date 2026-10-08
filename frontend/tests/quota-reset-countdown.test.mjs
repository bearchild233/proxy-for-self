import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 时间边界使用内置 runner
import test from 'node:test'
import { quotaResetCountdown } from '../../plugins/accounts/ui/page/components/AccountQuotaSummaryCell/presenter.ts'

test('quota reset countdown respects timezone, unknown and expired data', () => {
  const now = Date.parse('2026-10-07T12:00:00Z')
  assert.equal(quotaResetCountdown('2026-10-09T18:00:00Z', now), '2天6小时后重置')
  assert.equal(quotaResetCountdown('2026-10-07T21:23:00+08:00', now), '1小时23分后重置')
  assert.equal(quotaResetCountdown('2026-10-07T12:00:01Z', now), '1分后重置')
  assert.equal(quotaResetCountdown('2026-10-07T12:00:00Z', now), '已到重置时间，待刷新')
  assert.equal(quotaResetCountdown(undefined, now), '重置时间未提供')
  assert.equal(quotaResetCountdown('invalid', now), '重置时间未提供')
})

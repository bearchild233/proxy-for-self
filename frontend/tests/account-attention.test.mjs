import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 快捷筛选边界使用内置 runner
import test from 'node:test'
import { accountNeedsAttention } from '../src/views/accounts/utils/accountAttention.ts'

const account = { enabled: true, provider: 'openai', authenticationKind: 'oauth', status: 'normal', errorReason: null, quota: { limitReached: false, windows: [] } }
test('A single low reported window is sufficient; unknown quota is not reported as low', () => {
  assert.equal(accountNeedsAttention(account, 'low_quota'), false)
  assert.equal(accountNeedsAttention({ ...account, quota: { windows: [{ usedPercent: null }, { usedPercent: 90 }] } }, 'low_quota'), true)
  assert.equal(accountNeedsAttention({ ...account, quota: { windows: [{ usedPercent: 89.9 }] } }, 'low_quota'), false)
})
test('Subscription expiry uses an actual date; unknown and expired subscriptions are distinct', () => {
  const now = Date.parse('2026-09-29T12:00:00Z')
  assert.equal(accountNeedsAttention(account, 'expiring', null, now), false)
  assert.equal(accountNeedsAttention(account, 'expiring', { expiresAt: '2026-10-02T12:00:00Z' }, now), true)
  assert.equal(accountNeedsAttention(account, 'expiring', { expiresAt: '2026-10-02T12:00:01Z' }, now), false)
  assert.equal(accountNeedsAttention(account, 'expiring', { expiresAt: '2026-09-29T11:00:00Z' }, now), false)
  assert.equal(accountNeedsAttention(account, 'subscription_unknown', { expiresAt: 'invalid' }, now), true)
})
test('Authorization filter does not confuse rate limits, banned or disabled accounts with renewable credentials', () => {
  assert.equal(accountNeedsAttention({ ...account, errorReason: 'credential_invalid' }, 'reauthorize'), true)
  assert.equal(accountNeedsAttention({ ...account, errorReason: 'account_banned' }, 'reauthorize'), false)
  assert.equal(accountNeedsAttention({ ...account, status: 'rate_limited' }, 'reauthorize'), false)
  assert.equal(accountNeedsAttention({ ...account, errorReason: 'access_token_expired', hasRefreshToken: true }, 'reauthorize'), false)
  assert.equal(accountNeedsAttention({ ...account, errorReason: 'access_token_expired', hasRefreshToken: false }, 'reauthorize'), true)
  assert.equal(accountNeedsAttention({ ...account, enabled: false, errorReason: 'credential_invalid' }, 'reauthorize'), false)
})

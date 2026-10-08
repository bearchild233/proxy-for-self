import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
// eslint-disable-next-line test/no-import-node-test -- 独立进程验证浏览器所在时区不会改变计算
import test from 'node:test'

const dateModule = new URL('../src/utils/date.ts', import.meta.url).href
const quotaModule = new URL('../../plugins/accounts/ui/page/components/AccountQuotaSummaryCell/presenter.ts', import.meta.url).href

for (const zone of ['Asia/Shanghai', 'America/Los_Angeles', 'UTC']) {
  test(`Chinese display and countdowns are independent of host ${zone}`, () => {
    const output = execFileSync(process.execPath, ['--input-type=module', '-e', `
      import { formatDateTime, formatTime, parseTimestamp, formatRelativeTime, chinaTimeRange } from ${JSON.stringify(dateModule)};
      import { quotaResetCountdown } from ${JSON.stringify(quotaModule)};
      const now = Date.parse('2026-10-07T12:00:00Z');
      console.log(JSON.stringify({
        display: formatDateTime(now),
        time: formatTime(now),
        legacy: formatDateTime('2026-10-07 20:30:00'),
        cooldownMinutes: (parseTimestamp('2026-10-07 20:30:00') - now) / 60000,
        explicitMinutes: (parseTimestamp('2026-10-07T20:30:00+08:00') - now) / 60000,
        utcMinutes: (parseTimestamp('2026-10-07T12:30:00Z') - now) / 60000,
        subscriptionHours: (Date.parse('2026-10-09T05:58:51Z') - now) / 3600000,
        recent: formatRelativeTime('2026-10-07 19:30:00', now),
        quota: quotaResetCountdown('2026-10-07T20:30:00+08:00', now),
        spring: quotaResetCountdown('2026-03-08T03:30:00-07:00', Date.parse('2026-03-08T01:30:00-08:00')),
        autumn: quotaResetCountdown('2026-11-01T01:30:00-08:00', Date.parse('2026-11-01T01:30:00-07:00')),
        today: chinaTimeRange(0, '2026-10-07T17:00:00Z'),
        springWeek: chinaTimeRange(6, '2026-03-09T02:00:00Z'),
        autumnMonth: chinaTimeRange(29, '2026-11-02T02:00:00Z'),
        dateOnly: parseTimestamp('2026-10-08'),
        invalid: [formatDateTime('invalid'), parseTimestamp(null)],
      }));
    `], { env: { ...process.env, TZ: zone }, encoding: 'utf8' })
    const actual = JSON.parse(output)
    assert.deepEqual(actual, {
      display: '2026-10-07 20:00:00',
      time: '20:00:00',
      legacy: '2026-10-07 20:30:00',
      cooldownMinutes: 30,
      explicitMinutes: 30,
      utcMinutes: 30,
      subscriptionHours: 41 + 58 / 60 + 51 / 3600,
      recent: '30 分钟前',
      quota: '30分后重置',
      spring: '1小时0分后重置',
      autumn: '1小时0分后重置',
      today: { startTime: '2026-10-07T16:00:00.000Z', endTime: '2026-10-07T17:00:00.000Z' },
      springWeek: { startTime: '2026-03-02T16:00:00.000Z', endTime: '2026-03-09T02:00:00.000Z' },
      autumnMonth: { startTime: '2026-10-03T16:00:00.000Z', endTime: '2026-11-02T02:00:00.000Z' },
      dateOnly: Date.parse('2026-10-07T16:00:00Z'),
      invalid: ['—', null],
    })
  })
}

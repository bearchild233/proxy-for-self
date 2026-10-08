import type { ConfigType } from 'dayjs'
import dayjs from 'dayjs'
import timezone from 'dayjs/plugin/timezone.js'
import utc from 'dayjs/plugin/utc.js'

dayjs.extend(utc)
dayjs.extend(timezone)

const DATE_TIME_FORMAT = 'YYYY-MM-DD HH:mm:ss'
const TIME_FORMAT = 'HH:mm:ss'
export const DISPLAY_TIME_ZONE = 'Asia/Shanghai'

/** 中国时间采用固定 UTC+8，避免浏览器夏令时参与日界线计算。 */
export function chinaDate(value: ConfigType = new Date()) {
  return normalizedDate(value).utcOffset(480)
}

export function chinaTimeRange(days: number, end: ConfigType = new Date()) {
  const timestamp = normalizedDate(end).valueOf()
  const dayMs = 86_400_000
  const chinaOffsetMs = 8 * 3_600_000
  // 固定偏移的日界线用 epoch 运算，跨设备夏令时日期时不调用本地 setDate/startOf。
  const dayStart = Math.floor((timestamp + chinaOffsetMs) / dayMs) * dayMs - chinaOffsetMs
  return {
    startTime: new Date(dayStart - days * dayMs).toISOString(),
    endTime: new Date(timestamp).toISOString(),
  }
}

export function formatDateTime(value: ConfigType = new Date(), fallback = '—', timeZone = DISPLAY_TIME_ZONE): string {
  const timestamp = normalizedDate(value)
  if (!timestamp.isValid())
    return fallback
  return (timeZone === DISPLAY_TIME_ZONE ? timestamp.utcOffset(480) : timestamp.tz(timeZone)).format(DATE_TIME_FORMAT)
}

export function formatTime(value: ConfigType = new Date(), fallback = '—'): string {
  const timestamp = chinaDate(value)
  return timestamp.isValid() ? timestamp.format(TIME_FORMAT) : fallback
}

export function parseTimestamp(value: ConfigType): number | null {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.valueOf() : null
}

export function formatRelativeTime(
  value: ConfigType,
  now: ConfigType = new Date(),
): string {
  const timestamp = normalizedDate(value)
  if (!timestamp.isValid())
    return '—'

  const elapsedSeconds = Math.max(0, normalizedDate(now).diff(timestamp, 'second'))
  if (elapsedSeconds < 60)
    return '刚刚'

  const elapsedMinutes = Math.floor(elapsedSeconds / 60)
  if (elapsedMinutes < 60)
    return `${elapsedMinutes} 分钟前`

  const elapsedHours = Math.floor(elapsedMinutes / 60)
  if (elapsedHours < 24)
    return `${elapsedHours} 小时前`

  return `${Math.floor(elapsedHours / 24)} 天前`
}

function normalizedDate(value: ConfigType) {
  if (typeof value !== 'string')
    return dayjs(value)
  const normalized = value.trim().replace(' ', 'T')
  // 兼容后端现有的北京时间字符串（如 rateLimitedUntil），不把它解释成设备本地时间。
  if (/^\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?)?$/.test(normalized))
    return dayjs(`${normalized.includes('T') ? normalized : `${normalized}T00:00:00`}+08:00`)
  return dayjs(normalized)
}

interface LeaseHeartbeatOptions {
  renew: () => Promise<unknown>
  unavailable: () => Promise<void>
}

/** 临时网络失败不卸载页面；唤醒或恢复网络时立即续期，慢请求不重叠。 */
export function maintainPluginLease(options: LeaseHeartbeatOptions, browser = { document, window, setTimeout, clearTimeout }) {
  let stopped = false
  let pending = false
  let timer: ReturnType<typeof setTimeout> | undefined
  const schedule = (delay: number) => {
    browser.clearTimeout(timer)
    if (!stopped)
      timer = browser.setTimeout(renew, delay)
  }
  async function renew() {
    if (stopped || pending)
      return
    browser.clearTimeout(timer)
    pending = true
    let delay = 30_000
    try {
      await options.renew()
    }
    catch (cause) {
      delay = 5_000
      const status = typeof cause === 'object' && cause !== null && 'status' in cause ? cause.status : undefined
      // 只有明确的版本或权限拒绝才核对目录，超时和 5xx 保留组件与草稿。
      if (!stopped && (status === 400 || status === 403)) {
        try {
          await options.unavailable()
        }
        catch { /* 目录暂时不可达时继续静默重试。 */ }
      }
    }
    finally {
      pending = false
      schedule(delay)
    }
  }
  const resume = () => {
    if (!browser.document.hidden)
      void renew()
  }
  browser.document.addEventListener('visibilitychange', resume)
  browser.window.addEventListener('online', resume)
  browser.window.addEventListener('pageshow', resume)
  schedule(30_000)
  return () => {
    stopped = true
    browser.clearTimeout(timer)
    browser.document.removeEventListener('visibilitychange', resume)
    browser.window.removeEventListener('online', resume)
    browser.window.removeEventListener('pageshow', resume)
  }
}

import type { Ref } from 'vue'
import { onScopeDispose, watch } from 'vue'

// 仅可见页面轮询；上一轮完成后再计时，避免慢请求叠加。
export function useVisiblePolling(refresh: () => Promise<unknown>, interval: Readonly<Ref<number>>, visibility: Readonly<Ref<string>>) {
  let timer: ReturnType<typeof setTimeout> | undefined
  let running = false
  let disposed = false
  const enabled = () => !disposed && visibility.value === 'visible' && interval.value > 0

  function schedule(delay = interval.value) {
    clearTimeout(timer)
    if (enabled() && !running)
      timer = setTimeout(tick, delay)
  }

  async function tick() {
    if (!enabled() || running)
      return
    running = true
    try {
      await refresh()
    }
    catch {
      // 保留上次结果，下一轮重试；后台失败不弹出提示。
    }
    finally {
      running = false
      schedule()
    }
  }

  watch([visibility, interval], (_, previous) => {
    schedule(previous?.length ? 0 : interval.value)
  }, { immediate: true })
  onScopeDispose(() => {
    disposed = true
    clearTimeout(timer)
  })
}

import type { Ref } from 'vue'
import { onScopeDispose } from 'vue'

export function useTableExpansionMotion(
  viewport: () => HTMLElement | null | undefined,
  table: Ref<HTMLTableElement | null>,
  autoScroll: () => boolean | undefined,
) {
  const running = new Map<Element, () => void>()
  let scrollOwner: Element | undefined
  let previousAnchor = ''

  function animate(element: Element, entering: boolean, done: () => void) {
    running.get(element)?.()
    const row = element as HTMLTableRowElement
    const panel = row.querySelector<HTMLElement>('[data-expansion-panel]')
    const content = panel?.firstElementChild as HTMLElement | null
    const wrap = viewport()
    if (!panel || !content || !wrap) {
      done()
      return
    }
    const fromHeight = entering && !panel.style.height ? 0 : panel.getBoundingClientRect().height
    const toHeight = entering ? content.getBoundingClientRect().height : 0
    panel.style.height = `${fromHeight}px`
    const fromScroll = wrap.scrollTop
    const bounds = wrap.getBoundingClientRect()
    const inset = (table.value?.tHead?.getBoundingClientRect().height ?? 0) + 8
    const available = wrap.clientHeight - inset - 16
    const main = row.previousElementSibling!.getBoundingClientRect()
    const detailTop = row.getBoundingClientRect().top - bounds.top + fromScroll
    const mainTop = main.top - bounds.top + fromScroll
    const maxScroll = Math.max(0, wrap.scrollHeight + toHeight - fromHeight - wrap.clientHeight)
    let toScroll = fromScroll
    if (autoScroll()) {
      if (entering) {
        const start = main.height + toHeight <= available ? mainTop : detailTop
        toScroll = toHeight > available || start < fromScroll + inset
          ? start - inset
          : Math.max(fromScroll, detailTop + toHeight - wrap.clientHeight + 16)
      }
      else {
        toScroll = Math.min(fromScroll, Math.max(0, mainTop - inset))
      }
    }
    toScroll = Math.max(0, Math.min(maxScroll, toScroll))
    if (running.size === 0) {
      previousAnchor = wrap.style.overflowAnchor
      wrap.style.overflowAnchor = 'none'
    }
    scrollOwner = element
    let frame = 0
    const startTime = performance.now()
    const duration = window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 320

    function cleanup() {
      cancelAnimationFrame(frame)
      running.delete(element)
      if (running.size === 0)
        wrap!.style.overflowAnchor = previousAnchor
    }
    // 同一时间轴驱动详情高度与列表位置，避免收起后浏览器先钳制 scrollTop 而跳动。
    function tick(now: number) {
      const progress = duration ? Math.min(1, (now - startTime) / duration) : 1
      const eased = 1 - (1 - progress) ** 3
      panel!.style.height = `${fromHeight + (toHeight - fromHeight) * eased}px`
      if (scrollOwner === element)
        wrap!.scrollTop = fromScroll + (toScroll - fromScroll) * eased
      if (progress < 1) {
        frame = requestAnimationFrame(tick)
      }
      else {
        cleanup()
        panel!.style.height = entering ? '' : '0px'
        done()
      }
    }
    running.set(element, cleanup)
    frame = requestAnimationFrame(tick)
  }

  function cancel(element: Element) {
    // 保留中间高度，快速反向点击从当前位置继续。
    running.get(element)?.()
  }
  onScopeDispose(() => {
    for (const cancel of [...running.values()])
      cancel()
  })
  return {
    enter: (element: Element, done: () => void) => animate(element, true, done),
    leave: (element: Element, done: () => void) => animate(element, false, done),
    cancel,
  }
}

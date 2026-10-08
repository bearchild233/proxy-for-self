import type { Router } from 'vue-router'

// 每个 iframe 只承载一个固定页面；同包的其他路由也交给宿主复用驻留实例。
export function installHostNavigation(router: Router, pagePath: string, navigate: (path: string) => Promise<unknown>) {
  return router.beforeEach(async (to) => {
    if (to.path !== pagePath || !to.matched.length) {
      await navigate(to.fullPath)
      return false
    }
    return true
  })
}

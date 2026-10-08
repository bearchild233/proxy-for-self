interface PreloadPlugin {
  id: string
  contentVersion: string | null
  lazyLoad?: boolean
  pages: { id: string, route?: string, runtime?: string }[]
}
const routes = ['/', '/accounts', '/groups', '/keys', '/settings', '/usage', '/proxies', '/settings/pricing', '/settings/access', '/settings/backup', '/theme']

export function preloadPlan(plugins: PreloadPlugin[], currentPath: string) {
  const rank = (plugin: PreloadPlugin) => Math.min(...plugin.pages.map(page =>
    page.route === currentPath ? -1 : page.route && routes.includes(page.route) ? routes.indexOf(page.route) : 100))
  return plugins.filter(plugin => !plugin.lazyLoad && plugin.contentVersion)
    .sort((a, b) => rank(a) - rank(b))
    .flatMap(plugin => (plugin.pages[0]?.runtime === 'vue-component' ? plugin.pages.slice(0, 1) : plugin.pages)
      .map(page => ({ key: `${plugin.id}/${plugin.contentVersion}/${page.id}`, component: page.runtime === 'vue-component' })))
}

export async function runPreloads<T>(items: T[], load: (item: T) => Promise<void>, valid: () => boolean, concurrency = 3) {
  let index = 0
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, async () => {
    while (valid() && index < items.length) {
      const item = items[index++]!
      try {
        await load(item)
      }
      catch { /* 单包失败不阻塞其它预载，点击时仍可重试。 */ }
    }
  }))
}

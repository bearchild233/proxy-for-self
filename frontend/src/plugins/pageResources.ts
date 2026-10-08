import type { PlatformPlugin } from './platform'
import request from '@/api/request'
import { prepareComponent, retainComponentResources } from './componentResources'
import { preloadPlan, runPreloads } from './preloadPlan'
import { SharedResources } from './sharedResources'

const pages = new Map<string, string>()
const componentEntries = new Map<string, string>()
const unusedLeases = new Map<string, { lease: string, expiresAt: number, timer: ReturnType<typeof setTimeout> }>()
const pending = new Map<string, Promise<string>>()
let allowed = new Set<string>()
let generation = 0
let warming = false
let bytes = 0
const keyOf = (id: string, version: string, page: string) => `${id}/${version}/${page}`
const sharedLeases = new SharedResources<{ html: string, lease: string }>((key, value) => {
  void release(key, value.lease)
})
function release(key: string, lease: string) {
  const [id, version] = key.split('/')
  return request({ url: '/api/plugin-platform/lease', method: 'POST', data: { id, version, lease, release: true }, silent: true }).catch(() => {})
}
function releaseUnused(key: string) {
  const entry = unusedLeases.get(key)
  if (!entry)
    return
  unusedLeases.delete(key)
  clearTimeout(entry.timer)
  void release(key, entry.lease)
}
async function renewUnused(key: string) {
  const entry = unusedLeases.get(key)
  if (!entry)
    return
  if (!allowed.has(key) || document.hidden) {
    releaseUnused(key)
    return
  }
  const [id, version] = key.split('/')
  try {
    await request({ url: '/api/plugin-platform/lease', method: 'POST', data: { id, version, lease: entry.lease }, silent: true })
    if (unusedLeases.get(key) === entry) {
      entry.expiresAt = Date.now() + 105000
      entry.timer = setTimeout(renewUnused, 45000, key)
    }
  }
  catch {
    if (unusedLeases.get(key) === entry)
      releaseUnused(key)
  }
}
async function download(key: string) {
  if (pages.has(key))
    return pages.get(key)!
  const existing = pending.get(key)
  if (existing)
    return existing
  const epoch = generation
  const work = request<{ html: string, lease: string }>({ url: `/api/plugin-platform/page/${key}`, silent: true }).then(async (result) => {
    if (epoch !== generation || !allowed.has(key)) {
      void release(key, result.lease)
      throw new Error('页面授权或版本已变化')
    }
    // 页面下载已经取得租约，首次挂载直接接管；不再释放后额外请求一次。
    unusedLeases.set(key, { lease: result.lease, expiresAt: Date.now() + 105000, timer: setTimeout(renewUnused, 45000, key) })
    // 字符串内存按 UTF-16 预算，限制第三方扩展的大包占用。
    const size = result.html.length * 2
    if (bytes + size <= 32 * 1024 * 1024) {
      pages.set(key, result.html)
      bytes += size
    }
    return result.html
  }).finally(() => {
    if (pending.get(key) === work)
      pending.delete(key)
  })
  pending.set(key, work)
  return work
}
async function acquirePage(key: string, id: string, version: string) {
  const html = await download(key)
  const existing = unusedLeases.get(key)
  if (existing && existing.expiresAt > Date.now()) {
    unusedLeases.delete(key)
    clearTimeout(existing.timer)
    return { html, lease: existing.lease }
  }
  if (existing)
    releaseUnused(key)
  const { lease } = await request<{ lease: string }>({ url: '/api/plugin-platform/lease', method: 'POST', data: { id, version, lease: null }, silent: true })
  return { html, lease }
}
export async function loadPluginPage(id: string, version: string, page: string) {
  const componentKey = componentEntries.get(`${id}/${version}`)
  if (componentKey)
    return sharedLeases.acquire(componentKey, () => acquirePage(componentKey, id, version))
  const key = keyOf(id, version, page)
  const result = await acquirePage(key, id, version)
  return { ...result, release: () => {
    void release(key, result.lease)
  } }
}
export function resetPageResources() {
  generation++
  sharedLeases.clear()
  retainComponentResources(new Set(), true)
  for (const key of unusedLeases.keys()) releaseUnused(key)
  pages.clear()
  componentEntries.clear()
  pending.clear()
  allowed.clear()
  bytes = 0
}
export function syncPageResources(plugins: PlatformPlugin[]) {
  componentEntries.clear()
  for (const plugin of plugins) {
    if (plugin.contentVersion && plugin.pages[0]?.runtime === 'vue-component')
      componentEntries.set(`${plugin.id}/${plugin.contentVersion}`, keyOf(plugin.id, plugin.contentVersion, plugin.pages[0].id))
  }
  allowed = new Set(plugins.flatMap(plugin => plugin.contentVersion ? plugin.pages.map(page => keyOf(plugin.id, plugin.contentVersion!, page.id)) : []))
  for (const [key, html] of pages) {
    if (!allowed.has(key)) {
      releaseUnused(key)
      pages.delete(key)
      bytes -= html.length * 2
    }
  }
  retainComponentResources(new Set(plugins.filter(p => p.contentVersion).map(p => `${p.id}/${p.contentVersion}`)))
  if (warming || !allowed.size)
    return
  warming = true
  const epoch = generation
  const eager = preloadPlan(plugins, location.pathname)
  // 优先主要页面，最多三个静态包并行；不挂载业务页面或调用账号上游。
  setTimeout(async () => {
    try {
      await runPreloads(eager, async ({ key, component }) => {
        if (!allowed.has(key))
          return
        const source = await download(key)
        if (epoch !== generation || !allowed.has(key))
          return
        const [id, version] = key.split('/')
        if (component)
          await prepareComponent(`${id}/${version}`, source)
        // 让浏览器在连续初始化静态模块之间处理点击和绘制。
        await new Promise(resolve => setTimeout(resolve, 0))
      }, () => epoch === generation)
    }
    finally { warming = false }
  }, 0)
}
export async function prefetchPluginRoute(plugins: PlatformPlugin[], path: string) {
  const plugin = plugins.find(item => !item.lazyLoad && item.pages.some(page => page.route === path))
  if (!plugin?.contentVersion)
    return
  const page = plugin.pages.find(page => page.route === path)!
  const version = plugin.contentVersion
  const key = componentEntries.get(`${plugin.id}/${version}`) || keyOf(plugin.id, version, page.id)
  const epoch = generation
  try {
    if (!allowed.has(key))
      return
    const source = await download(key)
    if (epoch === generation && allowed.has(key) && page.runtime === 'vue-component')
      await prepareComponent(`${plugin.id}/${version}`, source)
  }
  catch { /* 点击仍走正常错误处理，不打扰悬停。 */ }
}

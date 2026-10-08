import { shallowRef } from 'vue'
import request from '@/api/request'
import { resetPageResources, syncPageResources } from './pageResources'

export interface PlatformPage { id: string, title: string, route?: string, runtime?: 'iframe' | 'vue-component', layout?: 'settings', slot?: string, roles: string[] }
export interface PluginRelease { version: string, contentVersion: string, releasedAt: string | null, notes: string[] }
export interface PlatformPlugin { lazyLoad?: boolean, id: string, name: string, description: string, category: string, required: boolean, enabled: boolean, installed: boolean, bundled: boolean, version: string | null, contentVersion: string | null, canRollback: boolean, currentRelease?: PluginRelease | null, previousRelease?: PluginRelease | null, operation?: { status: string, message: string }, capabilities: string[], pages: PlatformPage[] }
export const platformPlugins = shallowRef<PlatformPlugin[]>([])
export const pluginPopup = shallowRef<{ plugin: PlatformPlugin, page: PlatformPage, context: Record<string, unknown> }>()
let sessionEpoch = 0
export function resetPlatform() {
  sessionEpoch++
  platformPlugins.value = []
  resetPageResources()
}
export async function refreshPlatform() {
  const epoch = sessionEpoch
  const plugins = await request<PlatformPlugin[]>({ url: '/api/plugin-platform/catalog' })
  if (epoch !== sessionEpoch)
    return
  // 目录内容未变化时不触发所有驻留页面的响应式更新。
  if (JSON.stringify(platformPlugins.value) !== JSON.stringify(plugins))
    platformPlugins.value = plugins
  syncPageResources(plugins)
}
export async function openPlugin(id: string, context: Record<string, unknown> = {}, pageId?: string) {
  await refreshPlatform()
  const plugin = platformPlugins.value.find(p => p.id === id)
  const page = plugin?.pages.find(p => pageId ? p.id === pageId : !p.route)
  if (!plugin || !page)
    throw new Error('插件未启用或当前身份不可使用')
  pluginPopup.value = { plugin, page, context }
}

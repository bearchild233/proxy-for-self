import { computed, shallowRef } from 'vue'
import request from '@/api/request'

export type PluginCategory = 'management' | 'account-tools' | 'client-access' | 'analytics' | 'extensions' | 'operations'
export interface PluginDetails { overview?: string, features?: string[], data?: string, disableEffect?: string, updateEffect?: string }
export interface PluginPage {
  id: string
  title: string
  slot: 'settings' | 'key-usage' | 'key-editor' | 'key-config' | 'navigation' | 'account-menu' | 'account-detail' | 'group-menu' | 'key-menu' | 'proxy-menu' | 'usage-detail' | 'dashboard'
}
export interface ActivePlugin { id: string, name: string, version: string, capabilities: string[], pages: PluginPage[], required?: boolean, category?: PluginCategory, details?: PluginDetails }
export const activePlugins = shallowRef<ActivePlugin[]>([])
let generation = 0
let pending: Promise<void> | undefined

export function clearPlugins() {
  generation++
  pending = undefined
  activePlugins.value = []
}

export function invalidatePlugin(id: string) {
  generation++
  pending = undefined
  activePlugins.value = activePlugins.value.filter(plugin => plugin.id !== id)
}

export async function refreshPlugins() {
  if (pending)
    return pending
  const current = generation
  pending = (async () => {
    try {
      const data = await request<ActivePlugin[]>({ url: '/api/plugins', method: 'GET', silent: true })
      if (current === generation)
        activePlugins.value = data
    }
    catch {
      if (current === generation)
        activePlugins.value = []
    }
    finally {
      if (current === generation)
        pending = undefined
    }
  })()
  return pending
}

export const excelAvailable = computed(() => activePlugins.value.some(plugin => plugin.id === 'excel-bridge' && plugin.capabilities.includes('excel-inference')))

export function pluginPage(id: string, name: string) {
  return request<{ html: string }>({ url: `/api/plugins/${encodeURIComponent(id)}/pages/${encodeURIComponent(name)}`, method: 'GET' })
}

export function pluginRpc(id: string, name: string, input: unknown) {
  return request<unknown>({ url: `/api/plugins/${encodeURIComponent(id)}/rpc/${encodeURIComponent(name)}`, method: 'POST', data: input })
}

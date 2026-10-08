import { computed, shallowRef } from 'vue'
import { context, events, host } from './ui'
interface Plugin { id: string, name: string, pages: { id: string, title: string, slot?: string, route?: string }[], capabilities: string[] }
export const activePlugins = shallowRef<Plugin[]>((context() as unknown as { plugins: Plugin[] }).plugins || [])
export const excelAvailable = computed(() => activePlugins.value.some(p => p.id === 'excel-bridge'))
export function enabled(id: string) { return activePlugins.value.some(p => p.id === id) }
export async function refreshPlugins() { activePlugins.value = await host<Plugin[]>('catalog', {}) }
export function clearPlugins() { activePlugins.value = [] }
export function pluginRpc(id: string, operation: string, input: unknown) { return host('plugin-rpc', { id, operation, input }) }
export function pluginPage() { throw new Error('页面由宿主加载') }
export type PluginPage = Plugin['pages'][number]

events().addEventListener('plugin-context', () => { activePlugins.value = (context() as unknown as { plugins: Plugin[] }).plugins || [] })

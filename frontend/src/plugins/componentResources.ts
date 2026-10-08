import type { Component } from 'vue'
import type { PluginBridge } from '../../../packages/plugin-sdk/ui'
import { componentRuntime } from './componentRuntime'

export interface ComponentModule {
  hostApi: number
  requirements: string[]
  css: string
  create: (runtime: unknown, bridge: PluginBridge & { events: EventTarget }) => Record<string, { component: Component, route: string, name: string }>
}
const modules = new Map<string, Promise<ComponentModule>>()
const pins = new Map<string, number>()
export function pinComponent(key: string) {
  pins.set(key, (pins.get(key) || 0) + 1)
  return () => {
    const count = (pins.get(key) || 1) - 1
    if (count)
      pins.set(key, count)
    else pins.delete(key)
  }
}
const styles = new Map<string, { element: HTMLStyleElement, owners: Set<string> }>()
// 仅执行模块定义，不创建页面实例；页面 setup/API 查询在首次进入时才发生。
export function prepareComponent(key: string, source: string) {
  const existing = modules.get(key)
  if (existing)
    return existing
  const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }))
  const pending = import(/* @vite-ignore */ url).then((module: ComponentModule) => {
    if (typeof module.create !== 'function' || typeof module.css !== 'string' || module.hostApi !== 1 || !Array.isArray(module.requirements) || module.requirements.some(name => !Object.hasOwn(componentRuntime.shared, name)))
      throw new Error('组件插件接口无效')
    if (modules.get(key) === pending && module.css) {
      let style = styles.get(module.css)
      if (!style) {
        const element = document.createElement('style')
        element.dataset.pluginStyles = 'component'
        element.textContent = module.css
        document.head.append(element)
        style = { element, owners: new Set() }
        styles.set(module.css, style)
      }
      style.owners.add(key)
    }
    return module
  }).catch((error) => {
    modules.delete(key)
    throw error
  }).finally(() => URL.revokeObjectURL(url))
  modules.set(key, pending)
  return pending
}
export function retainComponentResources(keys: Set<string>, reset = false) {
  if (reset)
    pins.clear()
  else keys = new Set([...keys, ...pins.keys()])
  for (const key of modules.keys()) {
    if (!keys.has(key))
      modules.delete(key)
  }
  for (const [css, style] of styles) {
    for (const owner of style.owners) {
      if (!keys.has(owner))
        style.owners.delete(owner)
    }
    if (!style.owners.size) {
      style.element.remove()
      styles.delete(css)
    }
  }
}

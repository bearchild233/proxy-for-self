/// <reference path="./component-modules.d.ts" />
import bridge from 'proxy:bridge'
export type { PluginBridge } from './ui'
export function context() { return bridge.context }
export function events() { return bridge.events }
export function host<T = unknown>(operation: string, input: unknown): Promise<T> {
  return bridge.host<T>(operation, JSON.parse(JSON.stringify(input ?? {})))
}
export function call<T>(operation: string, input: unknown): Promise<T> {
  return bridge.call<T>(operation, JSON.parse(JSON.stringify(input ?? {})))
}

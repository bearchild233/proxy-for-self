export interface PluginBridge {
  call: <T>(operation: string, input: unknown) => Promise<T>
  context: { route: string, active?: boolean, settingsShell?: boolean, presentation?: 'page' | 'popup', role: 'admin' | 'key', apiBaseUrl: string, theme?: Record<string, string>, accountId?: string, accountName?: string, provider?: string, keyId?: string, apiKey?: string }
  host: <T>(operation: string, input: unknown) => Promise<T>
}

export function events(): EventTarget { return window }

export function context() {
  return (window as unknown as { proxyPlugin: PluginBridge }).proxyPlugin.context
}

export function host<T = unknown>(operation: string, input: unknown): Promise<T> {
  return (window as unknown as { proxyPlugin: PluginBridge }).proxyPlugin.host<T>(operation, JSON.parse(JSON.stringify(input ?? {})))
}

export function call<T>(operation: string, input: unknown): Promise<T> {
  const bridge = (window as unknown as { proxyPlugin?: PluginBridge }).proxyPlugin
  if (!bridge)
    return Promise.reject(new Error('插件必须在宿主中打开'))
  return bridge.call<T>(operation, JSON.parse(JSON.stringify(input ?? {})))
}

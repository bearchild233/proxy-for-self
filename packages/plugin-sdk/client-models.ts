import { call } from './ui'
export async function getClientModels(_baseUrl: string, apiKey: string, signal: AbortSignal): Promise<string[]> {
  signal.throwIfAborted()
  const result = await call<string[]>('client.models', { apiKey })
  signal.throwIfAborted()
  return result
}

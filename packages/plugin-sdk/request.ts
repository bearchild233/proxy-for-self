import type { RequestOptions } from '../../frontend/src/api/request'
import { ApiError } from '../../frontend/src/api/error'
import { toast } from '../../frontend/src/components/base/BaseToast'
import operations from './operations.json'
import { call } from './ui'

export type { RequestOptions }
export { ApiError }
const routes = new Map(Object.entries(operations).map(([name, spec]) => [`${spec.method} ${spec.path}`, name]))

export default async function request<T = unknown>(config: { url?: string, method?: string, params?: unknown, data?: unknown } & RequestOptions): Promise<T> {
  const operation = routes.get(`${(config.method || 'GET').toUpperCase()} ${config.url}`)
  if (!operation)
    throw new Error('插件未声明此接口')
  config.signal?.throwIfAborted()
  try {
    const result = await call<T>(operation, config.data ?? config.params ?? {})
    config.signal?.throwIfAborted()
    return result
  }
  catch (error) {
    if (!config.silent && !config.signal?.aborted)
      toast.error(error instanceof Error ? error.message : '操作失败')
    throw error
  }
}

export function setUnauthorizedHandler() {}
export function resetUnauthorizedHandling() {}

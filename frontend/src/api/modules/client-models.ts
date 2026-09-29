// 客户端模型目录使用当前分发 Key 鉴权，不使用管理员会话或管理 API 的响应包装。
export async function getClientModels(baseUrl: string, apiKey: string, signal: AbortSignal): Promise<string[]> {
  const response = await fetch(`${baseUrl.replace(/\/+$/, '')}/models`, {
    headers: { Authorization: `Bearer ${apiKey}` },
    credentials: 'omit',
    cache: 'no-store',
    signal,
  })
  if (!response.ok)
    throw new Error('模型目录加载失败')
  const body: unknown = await response.json()
  if (!body || typeof body !== 'object' || !('data' in body) || !Array.isArray(body.data))
    throw new Error('模型目录格式无效')
  return [...new Set(body.data.flatMap((item: unknown) => {
    if (item && typeof item === 'object' && 'id' in item && typeof item.id === 'string' && item.id.trim())
      return [item.id]
    return []
  }))].sort()
}

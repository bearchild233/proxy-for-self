export interface CachedPage { key: string, version: string | null, touched: number }

// 保持 DOM 顺序，避免移动 iframe 导致浏览器重新加载；仅淘汰最久未用页面。
export function retainPages<T extends CachedPage>(cached: T[], selected: T | undefined, available: Map<string, string | null>, limit = Number.POSITIVE_INFINITY): T[] {
  let result = cached.filter(item => available.has(item.key)
    && (item.key === selected?.key || item.version === available.get(item.key)))
  if (selected) {
    const existing = result.find(item => item.key === selected.key)
    result = existing
      ? result.map(item => item === existing ? { ...item, touched: selected.touched } : item)
      : [...result, selected]
  }
  while (result.length > limit) {
    const oldest = result.filter(item => item.key !== selected?.key).sort((a, b) => a.touched - b.touched)[0]!
    result = result.filter(item => item !== oldest)
  }
  return result
}

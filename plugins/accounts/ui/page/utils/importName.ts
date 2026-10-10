/** 名称是本地展示信息；只修改账号条目，保留凭据和其余导入字段。 */
export function applyAccountImportName(document: Record<string, unknown>, value: string): Record<string, unknown> {
  const name = value.trim()
  if (!name)
    return document
  const rename = (entry: unknown) => entry && typeof entry === 'object' && !Array.isArray(entry) ? { ...entry, name } : entry
  if (Array.isArray(document.accounts))
    return { ...document, accounts: document.accounts.map(rename) }
  const data = document.data
  if (data && typeof data === 'object' && !Array.isArray(data) && 'accounts' in data && Array.isArray(data.accounts))
    return { ...document, data: { ...data, accounts: data.accounts.map(rename) } }
  return { ...document, name }
}

import { isRecord } from '@/utils/object'

export type OAuthExportFormat = 'proxy' | 'codex'

interface OAuthAccountRef {
  id: string
  provider: string
  authenticationKind: string
  email?: string | null
}

/** 精确校验导出的账号，避免下载其它账号或将 API Key 当作 OAuth。 */
export function accountOAuthExport(payload: unknown, account: OAuthAccountRef, format: OAuthExportFormat) {
  if (account.authenticationKind !== 'oauth')
    throw new Error('仅 OAuth 账号可导出登录凭据')
  if (!isRecord(payload) || !Array.isArray(payload.documents) || payload.documents.length !== 1)
    throw new Error('导出的账号文档不完整')
  const document = payload.documents[0]
  if (!isRecord(document) || document.provider !== account.provider || !isRecord(document.document))
    throw new Error('导出的账号平台不匹配')
  const accounts = document.document.accounts
  if (!Array.isArray(accounts) || accounts.length !== 1 || !isRecord(accounts[0]))
    throw new Error('导出的账号文档不完整')
  const entry = accounts[0]
  if (stringField(entry, 'authentication_kind', 'authenticationKind') === 'api_key')
    throw new Error('导出的账号身份不匹配')
  if (account.provider === 'openai') {
    if (entry.id !== account.id || !stringField(entry, 'access_token', 'accessToken')?.trim())
      throw new Error('导出的账号身份不匹配')
  }
  else if (account.provider === 'xai') {
    // xAI 的规范 OAuth bundle 没有网关内部 id。账号范围由导出请求的单一
    // accountIds 和后端校验保证；仍校验平台、OAuth 类型和已有邮箱。
    const credentials = entry.credentials
    if (document.document.type !== 'oauth-account-bundle' || entry.platform !== 'grok'
      || entry.type !== 'oauth' || !isRecord(credentials) || !stringField(credentials, 'accessToken')?.trim())
      throw new Error('导出的账号文档不完整')
    const email = stringField(credentials, 'email')
    if (account.email && email && account.email !== email)
      throw new Error('导出的账号身份不匹配')
  }
  else {
    throw new Error('该平台暂不支持 OAuth 导出')
  }
  if (format === 'proxy')
    return payload
  if (account.provider !== 'openai')
    throw new Error('仅 OpenAI OAuth 账号支持 Codex 登录文件')
  // 网关 canonical 文档使用 camelCase，旧版代理导出使用 snake_case；两种都兼容，
  // 避免 UI 绑定到单一导出版本。
  const idToken = stringField(entry, 'id_token', 'idToken')
  const accessToken = stringField(entry, 'access_token', 'accessToken')
  const refreshToken = stringField(entry, 'refresh_token', 'refreshToken')
  const accountId = stringField(entry, 'accountId', 'account_id')
  if (![idToken, accessToken, refreshToken, accountId].every(value => typeof value === 'string' && value.trim()))
    throw new Error('缺少 ID Token、Refresh Token 或上游账号 ID，请重新授权后导出；当前凭据仍可导出为 OAuth JSON')
  // 文件只下载，不写入本机 auth.json，也不合并或替换现有 API Key。
  // 不伪造 last_refresh：账号 updatedAt 包含资料修改，并非令牌刷新时间。
  return {
    auth_mode: 'chatgpt',
    OPENAI_API_KEY: null,
    tokens: { id_token: idToken, access_token: accessToken, refresh_token: refreshToken, account_id: accountId },
  }
}

function stringField(value: Record<string, unknown>, ...keys: string[]) {
  for (const key of keys) {
    const candidate = value[key]
    if (typeof candidate === 'string')
      return candidate
  }
  return undefined
}

export function oauthExportFileName(account: OAuthAccountRef, format: OAuthExportFormat) {
  if (format === 'codex')
    return 'auth.json'
  const id = account.id.replace(/[^\w-]/g, '_')
  const provider = account.provider.replace(/[^\w-]/g, '_')
  return `${provider}-oauth-${id}.json`
}

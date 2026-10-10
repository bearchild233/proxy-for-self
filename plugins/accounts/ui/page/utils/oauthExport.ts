import { isRecord } from '@/utils/object'

export type OAuthExportFormat = 'proxy' | 'codex'

interface OAuthAccountRef {
  id: string
  provider: string
  authenticationKind: string
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
  if (entry.id !== account.id || entry.authentication_kind === 'api_key')
    throw new Error('导出的账号身份不匹配')
  if (format === 'proxy')
    return payload
  if (account.provider !== 'openai')
    throw new Error('仅 OpenAI OAuth 账号支持 Codex 登录文件')
  // OAuth provider 文档沿用 Rust credential 字段的 snake_case；公共账号字段使用 camelCase。
  const idToken = entry.id_token
  const accessToken = entry.access_token
  const refreshToken = entry.refresh_token
  const accountId = entry.accountId
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

export function oauthExportFileName(account: OAuthAccountRef, format: OAuthExportFormat) {
  if (format === 'codex')
    return 'auth.json'
  const id = account.id.replace(/[^\w-]/g, '_')
  const provider = account.provider.replace(/[^\w-]/g, '_')
  return `${provider}-oauth-${id}.json`
}

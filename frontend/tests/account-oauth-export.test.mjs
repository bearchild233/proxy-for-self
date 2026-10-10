/* eslint-disable no-new-func -- 只执行脱敏的导出格式校验，不访问网络或真实凭据 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'

const source = stripTypeScriptTypes(readFileSync(new URL('../../plugins/accounts/ui/page/utils/oauthExport.ts', import.meta.url), 'utf8'), { mode: 'strip' })
  .replace(/^import .*$/gm, '')
  .replace(/^export /gm, '')
const { accountOAuthExport, oauthExportFileName } = new Function('isRecord', `${source}\nreturn { accountOAuthExport, oauthExportFileName }`)(value => value !== null && typeof value === 'object' && !Array.isArray(value))

const account = { id: 'acct_fixture', provider: 'openai', authenticationKind: 'oauth' }
const document = {
  provider: 'openai',
  document: {
    accounts: [{
      id: 'acct_fixture',
      authentication_kind: 'oauth',
      id_token: 'id-token-fixture',
      access_token: 'access-token-fixture',
      refresh_token: 'refresh-token-fixture',
      accountId: 'chatgpt-account-fixture',
    }],
  },
}
const payload = { exportedAt: '2026-10-10T00:00:00Z', documents: [document] }

test('OAuth export validates identity and returns proxy JSON or native Codex auth.json', () => {
  assert.deepEqual(accountOAuthExport(payload, account, 'proxy'), payload)
  assert.deepEqual(accountOAuthExport(payload, account, 'codex'), {
    auth_mode: 'chatgpt',
    OPENAI_API_KEY: null,
    tokens: {
      id_token: 'id-token-fixture',
      access_token: 'access-token-fixture',
      refresh_token: 'refresh-token-fixture',
      account_id: 'chatgpt-account-fixture',
    },
  })
  assert.equal(oauthExportFileName(account, 'codex'), 'auth.json')
  assert.equal(oauthExportFileName(account, 'proxy'), 'openai-oauth-acct_fixture.json')
})

test('OAuth export rejects API keys, another account and incomplete tokens', () => {
  assert.throws(() => accountOAuthExport(payload, { ...account, authenticationKind: 'api_key' }, 'proxy'), /仅 OAuth/)
  assert.throws(() => accountOAuthExport(payload, { ...account, id: 'acct_other' }, 'proxy'), /身份不匹配/)
  assert.throws(() => accountOAuthExport({ ...payload, documents: [{ ...document, document: { accounts: [{ ...document.document.accounts[0], refresh_token: null }] } }] }, account, 'codex'), /缺少 ID Token/)
  assert.throws(() => accountOAuthExport(payload, { ...account, provider: 'xai' }, 'codex'), /平台不匹配/)
})

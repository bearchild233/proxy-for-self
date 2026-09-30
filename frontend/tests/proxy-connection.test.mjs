import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 使用现有 Node runner
import test from 'node:test'
import { buildProxyConnection, emptyProxyConnection, parseProxyConnection } from '../src/utils/proxyConnection.ts'

test('form encodes credentials and preserves IPv6 and Hysteria authentication', () => {
  for (const protocol of ['http', 'socks5h', 'hysteria2']) {
    const fields = { ...emptyProxyConnection(), protocol, host: '::1', port: '443', username: 'a@b', password: 'a:b@c%#/? 中文' }
    const parsed = parseProxyConnection(buildProxyConnection(fields))
    assert.equal(parsed.password, fields.password)
    assert.equal(parsed.host, '[::1]')
    if (protocol !== 'hysteria2')
      assert.equal(parsed.username, fields.username)
  }
})

test('VLESS forms preserve both Reality and WebSocket settings', () => {
  const fields = { ...emptyProxyConnection(), protocol: 'vless', host: 'example.invalid', port: '443', uuid: 'bf000d23-0752-40b4-affe-68f7707a9661', sni: 'example.com', publicKey: 'A'.repeat(43), shortId: 'aabb' }
  const reality = parseProxyConnection(buildProxyConnection(fields))
  assert.equal(reality.publicKey, fields.publicKey)
  assert.equal(reality.flow, 'xtls-rprx-vision')
  const ws = parseProxyConnection(buildProxyConnection({ ...fields, transport: 'ws', security: 'tls', path: '/a?b=c', wsHost: 'cdn.example.com' }))
  assert.equal(ws.path, '/a?b=c')
  assert.equal(ws.wsHost, 'cdn.example.com')
  assert.equal(ws.flow, '')
})

test('unsupported link parameters are never silently dropped by form conversion', () => {
  for (const query of ['mport=1234', 'insecure=1&insecure=0', 'pinSHA256=secret'])
    assert.throws(() => parseProxyConnection(`hy2://pass@example.com:443?${query}`))
  assert.throws(() => buildProxyConnection({ ...emptyProxyConnection(), host: 'host', port: '0' }))
})

test('VLESS UDP flags survive link to form round trips', () => {
  const prefix = 'vless://bf000d23-0752-40b4-affe-68f7707a9661@example.invalid:443?'
  for (const value of ['true', 'false', '1', '0']) {
    const fields = parseProxyConnection(`${prefix}udp=${value}`)
    assert.equal(new URL(buildProxyConnection(fields)).searchParams.get('udp'), ['1', 'true'].includes(value) ? 'true' : 'false')
  }
  assert.throws(() => parseProxyConnection(`${prefix}udp=maybe`))
})

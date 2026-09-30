export function emptyProxyConnection() {
  return {
    protocol: 'http',
    host: '',
    port: '',
    username: '',
    password: '',
    uuid: '',
    transport: 'tcp',
    udp: '',
    security: 'reality',
    flow: 'xtls-rprx-vision',
    sni: '',
    publicKey: '',
    shortId: '',
    fingerprint: 'chrome',
    path: '/',
    wsHost: '',
    alpn: '',
    insecure: false,
    obfs: '',
    obfsPassword: '',
  }
}

export type ProxyConnection = ReturnType<typeof emptyProxyConnection>

export function buildProxyConnection(fields: ProxyConnection): string {
  const host = fields.host.trim().replace(/^\[|\]$/g, '')
  if (!host || /[\s/@?#]/.test(host))
    throw new Error('请填写服务器域名或 IP，不要包含协议和路径')
  const port = Number(fields.port)
  if (!Number.isInteger(port) || port < 1 || port > 65535)
    throw new Error('端口应为 1–65535')
  const url = new URL(`${fields.protocol}://${host.includes(':') ? `[${host}]` : host}:${port}`)
  if (fields.protocol === 'vless') {
    if (!/^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i.test(fields.uuid.trim()))
      throw new Error('请填写有效的 VLESS UUID')
    url.username = fields.uuid.trim()
    url.searchParams.set('encryption', 'none')
    if (fields.udp) {
      if (!['true', 'false'].includes(fields.udp))
        throw new Error('UDP 参数无效')
      url.searchParams.set('udp', fields.udp)
    }
    url.searchParams.set('type', fields.transport)
    url.searchParams.set('security', fields.security)
    if (fields.transport === 'ws') {
      if (fields.security === 'reality')
        throw new Error('WebSocket 请使用 TLS 或无 TLS')
      if (!fields.path.startsWith('/'))
        throw new Error('WebSocket 路径应以 / 开头')
      url.searchParams.set('path', fields.path)
      if (fields.wsHost.trim())
        url.searchParams.set('host', fields.wsHost.trim())
    }
    else if (fields.flow && fields.security !== 'none') {
      url.searchParams.set('flow', fields.flow)
    }
    if (fields.security === 'reality') {
      if (!fields.sni.trim() || !/^[\w-]{43}$/.test(fields.publicKey.trim()) || !/^(?:[\da-f]{2}){0,8}$/i.test(fields.shortId.trim()))
        throw new Error('Reality 需要 SNI、43 位公钥和至多 16 位偶数长度十六进制 Short ID（可留空）')
      url.searchParams.set('pbk', fields.publicKey.trim())
      url.searchParams.set('sid', fields.shortId.trim())
    }
    if (fields.security !== 'none')
      url.searchParams.set('fp', fields.fingerprint)
  }
  else if (fields.protocol === 'hysteria2') {
    if (!fields.password)
      throw new Error('请填写 Hysteria2 认证密码')
    url.username = encodeURIComponent(fields.password)
    if (fields.obfs) {
      if (!fields.obfsPassword)
        throw new Error('请填写 Salamander 混淆密码')
      url.searchParams.set('obfs', fields.obfs)
      url.searchParams.set('obfs-password', fields.obfsPassword)
    }
  }
  else {
    if (fields.password && !fields.username)
      throw new Error('填写密码时也需要填写用户名')
    url.username = encodeURIComponent(fields.username)
    url.password = encodeURIComponent(fields.password)
    return url.toString()
  }
  if (fields.protocol === 'hysteria2' || fields.security !== 'none') {
    if (fields.sni.trim())
      url.searchParams.set('sni', fields.sni.trim())
    if (fields.alpn.trim())
      url.searchParams.set('alpn', fields.alpn.trim())
    if (fields.insecure)
      url.searchParams.set('insecure', '1')
  }
  return url.toString()
}

export function parseProxyConnection(value: string): ProxyConnection {
  const url = new URL(value)
  const fields = emptyProxyConnection()
  fields.protocol = url.protocol.slice(0, -1).replace(/^hy2$/, 'hysteria2')
  if (!['http', 'https', 'socks5', 'socks5h', 'vless', 'hysteria2'].includes(fields.protocol))
    throw new Error('此链接协议暂不支持表单编辑')
  const allowed = fields.protocol === 'vless'
    ? ['encryption', 'type', 'security', 'flow', 'sni', 'fp', 'pbk', 'sid', 'path', 'host', 'alpn', 'insecure', 'allowInsecure', 'udp']
    : fields.protocol === 'hysteria2' ? ['sni', 'alpn', 'insecure', 'obfs', 'obfs-password'] : []
  const seen = new Set<string>()
  for (const key of url.searchParams.keys()) {
    if (!allowed.includes(key) || seen.has(key))
      throw new Error('链接包含表单不支持的参数，请继续使用链接方式')
    seen.add(key)
  }
  for (const key of ['insecure', 'allowInsecure', 'udp']) {
    if (url.searchParams.has(key) && !['0', '1', 'false', 'true'].includes(url.searchParams.get(key)!))
      throw new Error('链接的证书验证或 UDP 参数无效')
  }
  if (!['', '/'].includes(url.pathname) || (url.searchParams.get('encryption') || 'none') !== 'none')
    throw new Error('链接包含不支持的连接参数')
  fields.host = url.hostname
  fields.port = url.port || (fields.protocol === 'http' ? '80' : ['https', 'hysteria2'].includes(fields.protocol) ? '443' : '')
  fields.username = decodeURIComponent(url.username)
  fields.password = fields.protocol === 'hysteria2'
    ? decodeURIComponent(value.slice(value.indexOf('://') + 3).split('@')[0]!)
    : decodeURIComponent(url.password)
  fields.uuid = decodeURIComponent(url.username)
  fields.transport = url.searchParams.get('type') || 'tcp'
  fields.udp = url.searchParams.has('udp') ? (['1', 'true'].includes(url.searchParams.get('udp')!) ? 'true' : 'false') : ''
  fields.security = url.searchParams.get('security') || 'tls'
  fields.flow = url.searchParams.get('flow') || ''
  fields.sni = url.searchParams.get('sni') || ''
  fields.publicKey = url.searchParams.get('pbk') || ''
  fields.shortId = url.searchParams.get('sid') || ''
  fields.fingerprint = url.searchParams.get('fp') || 'chrome'
  if (!['chrome', 'firefox', 'safari', 'ios', 'android', 'edge', '360', 'qq', 'random', 'randomized'].includes(fields.fingerprint))
    throw new Error('链接包含不支持的 TLS 指纹')
  fields.path = url.searchParams.get('path') || '/'
  fields.wsHost = url.searchParams.get('host') || ''
  fields.alpn = url.searchParams.get('alpn') || ''
  fields.insecure = ['1', 'true'].includes(url.searchParams.get('insecure') || url.searchParams.get('allowInsecure') || '')
  fields.obfs = url.searchParams.get('obfs') || ''
  fields.obfsPassword = url.searchParams.get('obfs-password') || ''
  if (!['tcp', 'ws'].includes(fields.transport) || !['none', 'tls', 'reality'].includes(fields.security)
    || !['', 'xtls-rprx-vision'].includes(fields.flow) || !['', 'salamander'].includes(fields.obfs)) {
    throw new Error('链接包含表单不支持的协议选项')
  }
  buildProxyConnection(fields)
  return fields
}

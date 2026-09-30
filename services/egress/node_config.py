"""将受支持的分享链接转换为 sing-box 出站；未知选项拒绝，避免静默改变出口。"""
import base64
import re
from urllib.parse import parse_qsl, unquote, urlsplit
from uuid import UUID


def outbound(uri):
    if len(uri) > 4096 or any(ord(c) < 32 or ord(c) == 127 for c in uri):
        raise ValueError('Invalid node')
    url = urlsplit(uri)
    scheme = 'hysteria2' if url.scheme == 'hy2' else url.scheme
    if scheme not in ('vless', 'hysteria2') or not url.hostname or url.path not in ('', '/'):
        raise ValueError('Invalid node')
    port = url.port or (443 if scheme == 'hysteria2' else 0)
    if not 1 <= port <= 65535 or not url.username:
        raise ValueError('Invalid node')
    pairs = parse_qsl(url.query, keep_blank_values=True, max_num_fields=24)
    query = dict(pairs)
    if len(query) != len(pairs) or any(any(ord(c) < 32 or ord(c) == 127 for c in value) for value in query.values()):
        raise ValueError('Invalid options')
    allowed = ({'type', 'security', 'encryption', 'flow', 'sni', 'fp', 'pbk', 'sid', 'path', 'host', 'alpn', 'insecure', 'allowInsecure', 'udp'}
               if scheme == 'vless' else {'sni', 'insecure', 'obfs', 'obfs-password', 'alpn'})
    if query.keys() - allowed:
        raise ValueError('Unsupported node options')
    for key in ('insecure', 'allowInsecure', 'udp'):
        if query.get(key, '0') not in ('0', '1', 'false', 'true'):
            raise ValueError('Invalid TLS option')
    tls = {'enabled': True, 'server_name': query.get('sni') or url.hostname,
           'insecure': query.get('insecure', query.get('allowInsecure', '0')) in ('1', 'true')}
    if query.get('alpn'):
        tls['alpn'] = query['alpn'].split(',')
    result = {'type': scheme, 'tag': 'egress', 'server': url.hostname, 'server_port': port}
    if scheme == 'hysteria2':
        result['password'] = unquote(url.netloc.rsplit('@', 1)[0])
        result['tls'] = tls
        if query.get('obfs', '') not in ('', 'salamander'):
            raise ValueError('Unsupported obfuscation')
        if query.get('obfs'):
            if not query.get('obfs-password'):
                raise ValueError('Missing obfuscation password')
            result['obfs'] = {'type': 'salamander', 'password': query['obfs-password']}
        return result
    if url.password is not None:
        raise ValueError('Invalid VLESS identity')
    result['uuid'] = str(UUID(url.username))
    # 分享链接的 UDP 开关不改变 TCP/HTTPS；false 时明确限制为 TCP。
    if query.get('udp') in ('0', 'false'):
        result['network'] = 'tcp'
    mode = query.get('type', 'tcp')
    security = query.get('security', 'tls')
    flow = query.get('flow', '')
    if mode not in ('tcp', 'ws') or security not in ('none', 'tls', 'reality') or query.get('encryption', 'none') != 'none':
        raise ValueError('Unsupported VLESS options')
    if flow not in ('', 'xtls-rprx-vision') or (flow and (mode != 'tcp' or security == 'none')):
        raise ValueError('Unsupported VLESS flow')
    if flow:
        result['flow'] = flow
    if security != 'none':
        fingerprint = query.get('fp', 'chrome')
        if fingerprint not in ('chrome', 'firefox', 'safari', 'ios', 'android', 'edge', '360', 'qq', 'random', 'randomized'):
            raise ValueError('Unsupported TLS fingerprint')
        tls['utls'] = {'enabled': True, 'fingerprint': fingerprint}
        if security == 'reality':
            key = query.get('pbk', '')
            short_id = query.get('sid', '')
            if mode != 'tcp' or not query.get('sni') or not re.fullmatch(r'[A-Za-z0-9_-]{43}', key):
                raise ValueError('Invalid Reality settings')
            if len(base64.urlsafe_b64decode(key + '=')) != 32 or not re.fullmatch(r'(?:[0-9a-fA-F]{2}){0,8}', short_id):
                raise ValueError('Invalid Reality identity')
            tls['reality'] = {'enabled': True, 'public_key': key, 'short_id': short_id}
        result['tls'] = tls
    if mode == 'ws':
        path = query.get('path', '/')
        if not path.startswith('/'):
            raise ValueError('Invalid WebSocket path')
        transport = {'type': 'ws', 'path': path}
        if query.get('host'):
            transport['headers'] = {'Host': query['host']}
        result['transport'] = transport
    return result


def configuration(node, port, password):
    return {
        'log': {'disabled': True},
        'dns': {'servers': [{'type': 'local', 'tag': 'local'}]},
        'inbounds': [{'type': 'http', 'tag': 'bridge', 'listen': '127.0.0.1', 'listen_port': port,
                      'users': [{'username': 'bridge', 'password': password}]}],
        'outbounds': [node],
        'route': {'final': 'egress', 'default_domain_resolver': 'local'},
    }

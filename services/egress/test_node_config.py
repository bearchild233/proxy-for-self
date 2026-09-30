import unittest
from node_config import configuration, outbound


class NodeConfigTests(unittest.TestCase):
    def test_hysteria_userpass_and_obfuscation(self):
        node = outbound('hy2://a%3Ab%40c@host?obfs=salamander&obfs-password=p%2B%23&sni=example.com')
        self.assertEqual(node['password'], 'a:b@c')
        self.assertEqual(node['server_port'], 443)
        self.assertEqual(node['obfs']['password'], 'p+#')
        self.assertFalse(node['tls']['insecure'])

    def test_ws_and_reality(self):
        prefix = 'vless://bf000d23-0752-40b4-affe-68f7707a9661@host:443?'
        ws = outbound(prefix + 'type=ws&security=tls&path=%2Ffoo&host=cdn.example.com')
        self.assertEqual(ws['transport']['headers']['Host'], 'cdn.example.com')
        self.assertEqual(ws['transport']['path'], '/foo')
        reality = outbound(prefix + 'security=reality&sni=example.com&pbk=' + 'A' * 43 + '&sid=aabb&flow=xtls-rprx-vision')
        self.assertEqual(reality['tls']['reality']['short_id'], 'aabb')
        self.assertEqual(reality['flow'], 'xtls-rprx-vision')

    def test_fail_closed_and_no_direct_outbound(self):
        for uri in ['hy2://pass@host?unknown=1', 'hy2://pass@host?insecure=maybe', 'hy2://pass@host?obfs=salamander', 'hy2://pass@host?sni=a&sni=b']:
            with self.assertRaises(ValueError):
                outbound(uri)
        config = configuration(outbound('hy2://pass@host'), 12345, 'test')
        self.assertEqual(config['route']['final'], 'egress')
        self.assertEqual(len(config['outbounds']), 1)
        self.assertEqual(config['inbounds'][0]['listen'], '127.0.0.1')

    def test_vless_udp_share_flags_preserve_tcp_and_validate_boolean(self):
        prefix = 'vless://bf000d23-0752-40b4-affe-68f7707a9661@host:443?'
        for flag in ('true', '1', 'false', '0'):
            node = outbound(prefix + 'udp=' + flag)
            self.assertEqual(node.get('network'), 'tcp' if flag in ('false', '0') else None)
        for flag in ('maybe', '', 'true&udp=false'):
            with self.assertRaises(ValueError):
                outbound(prefix + 'udp=' + flag)


if __name__ == '__main__':
    unittest.main()

"""可选真实协议集成验证：EGRESS_TEST_BINARY 指向本机 sing-box，所有节点均在回环地址。"""
import asyncio
import base64
from contextlib import suppress
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import unittest

from bridge import Bridge, Manager
from node_config import configuration, outbound


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


@unittest.skipUnless(os.environ.get('EGRESS_TEST_BINARY'), 'Set EGRESS_TEST_BINARY for local protocol test')
class BridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.binary = os.environ['EGRESS_TEST_BINARY']
        self.manager = Manager(self.binary, max_nodes=2)
        self.processes = []
        self.temp = tempfile.TemporaryDirectory()
        self.cert = str(Path(self.temp.name) / 'cert.pem')
        self.key = str(Path(self.temp.name) / 'key.pem')
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                        '-subj', '/CN=localhost', '-keyout', self.key, '-out', self.cert],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        async def echo(reader, writer):
            try:
                while data := await reader.read(4096):
                    writer.write(data)
                    await writer.drain()
            finally:
                writer.close()
        self.echo = await asyncio.start_server(echo, '127.0.0.1', 0)
        self.target = f'127.0.0.1:{self.echo.sockets[0].getsockname()[1]}'
        self.bridge = await asyncio.start_server(Bridge(self.manager).handle, '127.0.0.1', 0, limit=16384)
        self.bridge_port = self.bridge.sockets[0].getsockname()[1]

    async def asyncTearDown(self):
        self.bridge.close()
        self.echo.close()
        await self.bridge.wait_closed()
        await self.echo.wait_closed()
        await self.manager.close()
        for process in self.processes:
            if process.returncode is None:
                process.terminate()
            await process.wait()
        self.temp.cleanup()

    async def start_node(self, protocol):
        port = free_port()
        uid = 'bf000d23-0752-40b4-affe-68f7707a9661'
        inbound = {'type': 'hysteria2' if protocol == 'hy2' else 'vless', 'listen': '127.0.0.1', 'listen_port': port}
        inbound['users'] = [{'password': 'test-only'}] if protocol == 'hy2' else [{'uuid': uid}]
        if protocol in ('hy2', 'ws'):
            inbound['tls'] = {'enabled': True, 'certificate_path': self.cert, 'key_path': self.key}
        if protocol == 'ws':
            inbound['transport'] = {'type': 'ws', 'path': '/test'}
        config = {'log': {'disabled': True}, 'inbounds': [inbound], 'outbounds': [{'type': 'direct'}]}
        process = await asyncio.create_subprocess_exec(self.binary, 'run', '-c', '/dev/stdin', stdin=asyncio.subprocess.PIPE,
                                                     stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
        self.processes.append(process)
        process.stdin.write(json.dumps(config).encode())
        await process.stdin.drain()
        process.stdin.close()
        await asyncio.sleep(0.4)
        self.assertIsNone(process.returncode)
        if protocol == 'hy2':
            return f'hy2://test-only@127.0.0.1:{port}?insecure=1&sni=localhost'
        query = 'security=none' if protocol == 'tcp' else 'type=ws&security=tls&insecure=1&sni=localhost&path=%2Ftest'
        return f'vless://{uid}@127.0.0.1:{port}?{query}'

    async def connect(self, uri):
        reader, writer = await asyncio.open_connection('127.0.0.1', self.bridge_port)
        encoded = base64.urlsafe_b64encode(uri.encode()).rstrip(b'=').decode()
        token = base64.b64encode(f'{encoded}:node'.encode()).decode()
        writer.write(f'CONNECT {self.target} HTTP/1.1\r\nProxy-Authorization: Basic {token}\r\n\r\n'.encode())
        await writer.drain()
        header = await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'), 12)
        return header, reader, writer

    async def test_real_vless_tcp_websocket_tls_and_hysteria2(self):
        for protocol in ('tcp', 'ws', 'hy2'):
            uri = await self.start_node(protocol)
            header, reader, writer = await self.connect(uri)
            self.assertIn(b'200', header, protocol)
            for turn in range(3):
                data = f'{protocol}:turn-{turn}'.encode()
                writer.write(data)
                await writer.drain()
                self.assertEqual(await asyncio.wait_for(reader.readexactly(len(data)), 3), data)
            writer.close()
            await writer.wait_closed()
            await asyncio.sleep(0.05)
        # 非法节点仍然返回失败，目标虽然直连可达也不能透传。
        header, _, writer = await self.connect('hy2://pass@127.0.0.1:1?unsupported=1')
        self.assertIn(b'502', header)
        writer.close()
        await writer.wait_closed()

    async def test_pool_reuses_nodes_and_never_evicts_active_tunnels(self):
        first = await self.start_node('tcp')
        second = await self.start_node('hy2')
        a = await self.manager.acquire(first)
        reused = await self.manager.acquire(first)
        self.assertIs(a, reused)
        b = await self.manager.acquire(second)
        with self.assertRaises(RuntimeError):
            await self.manager.acquire('hy2://other@127.0.0.1:12345')
        self.assertIsNone(a.process.returncode)
        self.manager.release(a)
        self.manager.release(reused)
        self.manager.release(b)

    async def test_reality_config_accepted_by_pinned_runtime(self):
        uri = 'vless://bf000d23-0752-40b4-affe-68f7707a9661@127.0.0.1:443?security=reality&sni=example.com&pbk=' + 'A' * 43 + '&sid=aabb&flow=xtls-rprx-vision'
        config = configuration(outbound(uri), free_port(), 'test-only')
        process = await asyncio.create_subprocess_exec(self.binary, 'check', '-c', '/dev/stdin', stdin=asyncio.subprocess.PIPE,
                                                     stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        _, error = await process.communicate(json.dumps(config).encode())
        self.assertEqual(process.returncode, 0, error.decode())

    async def test_real_reality_tunnel(self):
        generator = await asyncio.create_subprocess_exec(self.binary, 'generate', 'reality-keypair', stdout=asyncio.subprocess.PIPE)
        raw, _ = await generator.communicate()
        keys = dict(line.split(':', 1) for line in raw.decode().strip().splitlines())
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(self.cert, self.key)
        async def camouflage(reader, writer):
            with suppress(Exception):
                await asyncio.wait_for(reader.read(1024), 3)
            writer.close()
        target = await asyncio.start_server(camouflage, '127.0.0.1', 0, ssl=context)
        port = free_port()
        uid = 'bf000d23-0752-40b4-affe-68f7707a9661'
        config = {'log': {'disabled': True}, 'inbounds': [{
            'type': 'vless', 'listen': '127.0.0.1', 'listen_port': port,
            'users': [{'uuid': uid, 'flow': 'xtls-rprx-vision'}],
            'tls': {'enabled': True, 'server_name': 'localhost', 'reality': {
                'enabled': True, 'handshake': {'server': '127.0.0.1', 'server_port': target.sockets[0].getsockname()[1]},
                'private_key': keys['PrivateKey'].strip(), 'short_id': ['aabb'],
            }},
        }], 'outbounds': [{'type': 'direct'}]}
        process = await asyncio.create_subprocess_exec(self.binary, 'run', '-c', '/dev/stdin', stdin=asyncio.subprocess.PIPE,
                                                     stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
        self.processes.append(process)
        process.stdin.write(json.dumps(config).encode())
        await process.stdin.drain()
        process.stdin.close()
        try:
            await asyncio.sleep(0.4)
            uri = f'vless://{uid}@127.0.0.1:{port}?security=reality&sni=localhost&pbk={keys["PublicKey"].strip()}&sid=aabb&flow=xtls-rprx-vision'
            header, reader, writer = await self.connect(uri)
            self.assertIn(b'200', header)
            writer.write(b'reality-test')
            await writer.drain()
            self.assertEqual(await asyncio.wait_for(reader.readexactly(12), 5), b'reality-test')
            writer.close()
            await writer.wait_closed()
        finally:
            target.close()
            await target.wait_closed()


if __name__ == '__main__':
    unittest.main()

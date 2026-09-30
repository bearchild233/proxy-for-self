"""本机 HTTP CONNECT 桥接。每节点独立 sing-box，已有连接不因增删节点重启。

节点 URI 只在回环认证头、进程内存和子进程标准输入中传递；不记录认证头或原始异常。
"""
import asyncio
import base64
from contextlib import suppress
from dataclasses import dataclass
import hashlib
import json
import os
import secrets
import signal
import socket
import time

from node_config import configuration, outbound


@dataclass
class Node:
    process: asyncio.subprocess.Process
    port: int
    password: str
    active: int = 0
    last_used: float = 0


class Manager:
    def __init__(self, binary, max_nodes=2):
        self.binary = binary
        self.max_nodes = max_nodes
        self.nodes = {}
        self.lock = asyncio.Lock()

    async def stop(self, node):
        if node.process.returncode is None:
            with suppress(ProcessLookupError):
                node.process.terminate()
            try:
                await asyncio.wait_for(node.process.wait(), 3)
            except asyncio.TimeoutError:
                with suppress(ProcessLookupError):
                    node.process.kill()
                await node.process.wait()

    async def acquire(self, uri):
        config = outbound(uri)
        # 按实际节点配置去重；URL 参数顺序和备注不触发另起实例。
        key = hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()
        async with self.lock:
            node = self.nodes.get(key)
            if node and node.process.returncode is None:
                node.active += 1
                return node
            self.nodes.pop(key, None)
            if len(self.nodes) >= self.max_nodes:
                idle = [(k, n) for k, n in self.nodes.items() if n.active == 0]
                if not idle:
                    raise RuntimeError('Node capacity reached')
                old_key, old_node = min(idle, key=lambda item: item[1].last_used)
                await self.stop(old_node)
                del self.nodes[old_key]
            with socket.socket() as reserved:
                reserved.bind(('127.0.0.1', 0))
                port = reserved.getsockname()[1]
            password = secrets.token_urlsafe(32)
            process = await asyncio.create_subprocess_exec(
                self.binary, 'run', '-c', '/dev/stdin', stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL,
            )
            node = Node(process, port, password)
            try:
                process.stdin.write(json.dumps(configuration(config, port, password)).encode())
                await process.stdin.drain()
                process.stdin.close()
                for _ in range(80):
                    if process.returncode is not None:
                        raise RuntimeError('Node failed to start')
                    try:
                        _, writer = await asyncio.open_connection('127.0.0.1', port)
                        writer.close()
                        await writer.wait_closed()
                        break
                    except OSError:
                        await asyncio.sleep(0.05)
                else:
                    raise TimeoutError('Node start timeout')
                node.active = 1
                self.nodes[key] = node
                return node
            except BaseException:
                await self.stop(node)
                raise

    def release(self, node):
        node.active -= 1
        node.last_used = time.monotonic()

    async def reap(self):
        while True:
            await asyncio.sleep(30)
            async with self.lock:
                for key, node in list(self.nodes.items()):
                    if not node.active and time.monotonic() - node.last_used > 120:
                        await self.stop(node)
                        del self.nodes[key]

    async def close(self):
        for node in self.nodes.values():
            await self.stop(node)


async def relay(reader, writer):
    while chunk := await asyncio.wait_for(reader.read(65536), 600):
        writer.write(chunk)
        await asyncio.wait_for(writer.drain(), 30)


class Bridge:
    def __init__(self, manager):
        self.manager = manager
        self.active = 0

    async def handle(self, reader, writer):
        node = None
        upstream = None
        committed = False
        if self.active >= 64:
            writer.close()
            return
        self.active += 1
        deadline = asyncio.get_running_loop().call_later(10, asyncio.current_task().cancel)
        try:
            header = await reader.readuntil(b'\r\n\r\n')
            if len(header) > 16384:
                raise ValueError('Header too large')
            lines = header.decode('ascii').split('\r\n')
            request = lines[0].split(' ')
            if len(request) != 3 or request[0] != 'CONNECT' or request[2] != 'HTTP/1.1':
                raise ValueError('Only CONNECT is supported')
            # CONNECT 目标不能注入头或转换为 HTTP absolute-form。
            target = request[1]
            host, port = target.rsplit(':', 1)
            if not host or '/' in host or '@' in host or not port.isdigit() or not 1 <= int(port) <= 65535:
                raise ValueError('Invalid target')
            auth = [line.split(':', 1)[1].strip() for line in lines[1:] if line.lower().startswith('proxy-authorization:')]
            if len(auth) != 1 or not auth[0].startswith('Basic '):
                raise ValueError('Missing node authentication')
            credentials = base64.b64decode(auth[0][6:], validate=True).decode('ascii')
            encoded, password = credentials.rsplit(':', 1)
            if password != 'node' or len(encoded) > 5500:
                raise ValueError('Invalid node authentication')
            uri = base64.b64decode(encoded + '=' * (-len(encoded) % 4), altchars=b'-_', validate=True).decode('utf-8')
            node = await self.manager.acquire(uri)
            remote_reader, upstream = await asyncio.open_connection('127.0.0.1', node.port)
            token = base64.b64encode(f'bridge:{node.password}'.encode()).decode()
            # 只向子进程发送其随机认证信息，节点 URI 绝不转发到目标服务器。
            upstream.write(f'CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {token}\r\n\r\n'.encode())
            await upstream.drain()
            response = await remote_reader.readuntil(b'\r\n\r\n')
            if response.split(b'\r\n', 1)[0].split(b' ')[1:2] != [b'200']:
                raise RuntimeError('Node connection failed')
            writer.write(b'HTTP/1.1 200 Connection Established\r\n\r\n')
            await writer.drain()
            committed = True
            deadline.cancel()
            tasks = [asyncio.create_task(relay(reader, upstream)), asyncio.create_task(relay(remote_reader, writer))]
            try:
                await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)
        except (Exception, asyncio.CancelledError):
            if not committed:
                with suppress(Exception):
                    writer.write(b'HTTP/1.1 502 Egress unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n')
                    await writer.drain()
        finally:
            deadline.cancel()
            for connection in (upstream, writer):
                if connection:
                    connection.close()
                    with suppress(Exception):
                        await asyncio.wait_for(connection.wait_closed(), 2)
            if node:
                self.manager.release(node)
            self.active -= 1


async def main():
    manager = Manager(os.environ.get('SING_BOX_BINARY', '/opt/proxy-for-self-egress/sing-box'),
                      int(os.environ.get('EGRESS_MAX_NODES', '2')))
    bridge = Bridge(manager)
    server = await asyncio.start_server(bridge.handle, '127.0.0.1', 18323, limit=16384)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for signum in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(signum, stop.set)
    reap = asyncio.create_task(manager.reap())
    try:
        async with server:
            await stop.wait()
    finally:
        reap.cancel()
        await asyncio.gather(reap, return_exceptions=True)
        await manager.close()


if __name__ == '__main__':
    asyncio.run(main())

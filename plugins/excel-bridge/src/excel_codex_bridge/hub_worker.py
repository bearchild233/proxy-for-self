"""API Hub 的私有 UDS worker；身份只能由已鉴权的 Rust 网关提供。"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path
import stat
import signal
import socket
import time
from urllib.parse import urlsplit

import httpx
from starlette.requests import Request
from starlette.responses import JSONResponse

from . import codex_config, excel_upstream, sse
from .server import Bridge
from .request_body import BodyTooLarge, read_json

MAX_BODY_BYTES = 8 * 1024 * 1024
MAX_CONCURRENCY = 4
REQUEST_TIMEOUT = 600


class BoundReader:
    """不读本机认证、不刷新令牌、不尝试第二个账号。"""
    source = "codex"
    last_error = ""

    def __init__(self, store):
        self.store = store

    def refresh(self, **_):
        return self.store.status()

    def fall_back(self, _):
        return False

    def hint(self):
        return "Update the bound account session in API Hub."


def _identifier(value, name):
    if not isinstance(value, str) or not value or len(value) > 256:
        raise ValueError(f"Invalid {name}")
    if any(ord(ch) < 32 or ord(ch) == 127 for ch in value):
        raise ValueError(f"Invalid {name}")
    return value

def _prepare(envelope):
    binding = envelope.get("binding")
    credential = envelope.get("credential")
    body = envelope.get("body")
    if not isinstance(binding, dict) or not isinstance(credential, dict) or not isinstance(body, dict):
        raise ValueError("binding, credential and body objects are required")
    key = _identifier(binding.get("key_id"), "key_id")
    account = _identifier(binding.get("account_id"), "account_id")
    revision = binding.get("revision")
    if type(revision) is not int or revision < 1 or binding.get("excel_enabled") is not True:
        raise ValueError("An enabled, versioned Excel binding is required")
    if credential.get("account_id") != account:
        raise ValueError("Credential account does not match the frozen binding")
    upstream_account = _identifier(credential.get("chatgpt_account_id"), "chatgpt_account_id")
    token = credential.get("access_token")
    if not isinstance(token, str) or not token or len(token) > 32768 or any(ch.isspace() or ord(ch) < 32 or ord(ch) == 127 for ch in token):
        raise ValueError("Invalid access token")
    # 令牌轮换也隔离旧回放，宁可显式失败，不跨凭据代次续接。
    namespace = hashlib.sha256(json.dumps([key, account, revision, upstream_account,
        hashlib.sha256(token.encode()).hexdigest()], separators=(",", ":")).encode()).hexdigest()
    headers = {"authorization": f"Bearer {token}", "chatgpt-account-id": upstream_account,
               "x-openai-account-id": upstream_account, "x-basispoints-auth-mode": "chatgpt"}
    if credential.get("user_id") is not None:
        headers["x-openai-account-user-id"] = _identifier(credential["user_id"], "user_id")
    proxy = credential.get("proxy")
    if proxy is not None:
        if not isinstance(proxy, str) or len(proxy) > 4096:
            raise ValueError("Invalid outbound proxy")
        parsed = urlsplit(proxy)
        if parsed.scheme not in {"http", "https", "socks5", "socks5h"} or not parsed.hostname:
            raise ValueError("Invalid outbound proxy")
    # 原桥依赖完整历史，不转发 previous_response_id，不能悄悄丢弃续接上下文。
    if body.get("previous_response_id"):
        raise UnsupportedExcelInput("Excel Bridge requires full input history; previous_response_id is not supported")
    pending = [body.get("input")]
    while pending:
        item = pending.pop()
        if isinstance(item, dict):
            if item.get("type") == "input_file":
                raise UnsupportedExcelInput("Excel Bridge does not support input_file; send extracted text or input_image instead")
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)
    store = excel_upstream.ExcelSessionStore(None)
    store.configure(headers, persist=False)
    return namespace, store, body, proxy


class UnsupportedExcelInput(ValueError):
    """仅承载固定的能力说明，不包含账号、凭据或请求内容。"""


def upstream_client(proxy):
    return httpx.AsyncClient(
        timeout=httpx.Timeout(connect=30, read=REQUEST_TIMEOUT, write=60, pool=10),
        limits=httpx.Limits(max_connections=2, max_keepalive_connections=1),
        verify=True, follow_redirects=False, trust_env=False, proxy=proxy,
    )


class HubWorker:
    def __init__(self, *, client_factory=upstream_client, max_concurrency=MAX_CONCURRENCY,
                 body_limit=MAX_BODY_BYTES, timeout=REQUEST_TIMEOUT, idle_timeout=0):
        if max_concurrency < 1 or body_limit < 1 or timeout <= 0:
            raise ValueError("Worker limits must be positive")
        self.client_factory = client_factory
        self.max_concurrency = max_concurrency
        self.body_limit = body_limit
        self.timeout = timeout
        self.active = 0
        self.idle_timeout = idle_timeout
        self.last_activity = time.monotonic()
        # 专用进程不把用户的会话回放内容写到磁盘。
        excel_upstream.keep_native_calls_in(None)

    async def _idle_shutdown(self):
        while True:
            await asyncio.sleep(min(self.idle_timeout, 30))
            if self.active == 0 and time.monotonic() - self.last_activity >= self.idle_timeout:
                # 仅通知本进程优雅退出，监听 socket 仍由 systemd 持有。
                os.kill(os.getpid(), signal.SIGTERM)
                return

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            idle_task = None
            while True:
                event = await receive()
                if event["type"] == "lifespan.startup":
                    if self.idle_timeout > 0:
                        idle_task = asyncio.create_task(self._idle_shutdown())
                    await send({"type": "lifespan.startup.complete"})
                elif event["type"] == "lifespan.shutdown":
                    if idle_task is not None:
                        idle_task.cancel()
                        await asyncio.gather(idle_task, return_exceptions=True)
                    await send({"type": "lifespan.shutdown.complete"})
                    return
        if scope["type"] == "websocket":
            await send({"type": "websocket.close", "code": 1008})
            return
        if scope["type"] != "http":
            return
        # uvicorn 的 UDS 请求没有 client 地址；即便误以 --host 启动也拒绝 TCP。
        if scope.get("client") is not None or any(k.lower() == b"origin" for k, _ in scope.get("headers", [])):
            return await sse.openai_error_response(403, "Private Unix socket only")(scope, receive, send)
        path, method = scope["path"], scope["method"]
        if (path, method) == ("/healthz", "GET"):
            return await JSONResponse({"ok": True, "active": self.active})(scope, receive, send)
        if (path, method) == ("/internal/catalog", "GET"):
            return await JSONResponse(codex_config.catalog_payload())(scope, receive, send)
        if method != "POST" or path not in {
            "/internal/responses", "/internal/images/generations", "/internal/images/edits",
        }:
            return await sse.openai_error_response(404, "Not found")(scope, receive, send)
        if self.active >= self.max_concurrency:
            return await sse.openai_error_response(503, "Excel worker is at capacity")(scope, receive, send)
        self.active += 1
        self.last_activity = time.monotonic()
        started = False

        async def tracked_send(event):
            nonlocal started
            if event["type"] == "http.response.start":
                started = True
            await send(event)

        try:
            await asyncio.wait_for(self._serve(scope, receive, tracked_send), timeout=self.timeout)
        except asyncio.TimeoutError:
            if started:
                # SSE 已开始时终止连接，不伪造完成事件或重复 HTTP 响应头。
                raise
            await sse.openai_error_response(504, "Excel worker request timed out")(scope, receive, send)
        finally:
            self.active -= 1
            self.last_activity = time.monotonic()

    async def _serve(self, scope, receive, send):
        try:
            envelope = await read_json(Request(scope, receive), self.body_limit)
            namespace, store, body, proxy = _prepare(envelope)
        except BodyTooLarge:
            return await sse.openai_error_response(413, "Request body is too large")(scope, receive, send)
        except UnsupportedExcelInput as exc:
            return await sse.openai_error_response(400, str(exc), code="unsupported_excel_input")(scope, receive, send)
        except (ValueError, RecursionError):
            # 禁止把输入、令牌或代理 URL 拼进错误正文/日志。
            return await sse.openai_error_response(400, "Invalid or unsupported Excel worker request")(scope, receive, send)
        bridge = Bridge(BoundReader(store), lambda: self.client_factory(proxy))
        try:
            # ASGI 响应的整个生命周期都处于同一命名空间，包括流式回放缓存写入。
            with excel_upstream.scoped_native_calls(namespace):
                if scope["path"] == "/internal/responses":
                    response = await bridge.responses(body)
                else:
                    # 生图与改图复用原桥参数、PNG 限制和附件转换，不另造映射。
                    response = await bridge.images(scope["path"].rsplit("/", 1)[1], body)
                await response(scope, receive, send)
        finally:
            try:
                await bridge.aclose()
            finally:
                store.clear()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    binding = parser.add_mutually_exclusive_group(required=True)
    binding.add_argument("--socket", type=Path)
    binding.add_argument("--fd", type=int)
    parser.add_argument("--max-concurrency", type=int, default=MAX_CONCURRENCY)
    parser.add_argument("--idle-timeout", type=int, default=300)
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("This worker requires a POSIX Unix socket")
    if args.fd is not None:
        with socket.socket(fileno=os.dup(args.fd)) as inherited:
            if inherited.family != socket.AF_UNIX or inherited.type != socket.SOCK_STREAM:
                parser.error("Only a private Unix stream socket can be inherited")
            socket_path = Path(inherited.getsockname())
    else:
        socket_path = args.socket
    if args.idle_timeout < 0:
        parser.error("Idle timeout cannot be negative")
    parent = socket_path.parent
    info = parent.lstat()
    if not socket_path.is_absolute() or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        parser.error("Socket parent must be an owned private directory (0700)")
    if args.fd is None and (socket_path.exists() or socket_path.is_symlink()):
        parser.error("Socket path already exists; refusing to replace it")
    os.umask(0o077)
    import uvicorn
    binding = {"fd": args.fd} if args.fd is not None else {"uds": str(socket_path)}
    uvicorn.run(HubWorker(max_concurrency=args.max_concurrency, idle_timeout=args.idle_timeout), **binding,
                access_log=False, log_level="warning", server_header=False,
                timeout_keep_alive=5, limit_concurrency=args.max_concurrency + 4)


if __name__ == "__main__":
    main()

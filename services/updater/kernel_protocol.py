"""updater 与控制面共用的最小内核协议客户端（协议 0.7，只用管理连接）。

帧格式与 backend/crates/kernel-protocol 一致：4 字节大端长度 + UTF-8 JSON，单帧上限 1 MiB。
只实现管理连接需要的 hello、health、admin.call；管理连接不处理钩子和能力调用。
未知字段忽略，通知与不匹配的响应直接丢弃。
"""
from __future__ import annotations

import json
import socket
import struct
import time

MAX_FRAME_BYTES = 1_048_576
KERNEL_API_MAJOR = 0
KERNEL_API_MIN_MINOR = 4
KERNEL_API_MINOR = 7
# 握手中报告给插件的内核版本。TODO(design): 任务未规定握手版本字符串，此处使用固定占位值。
KERNEL_VERSION = "updater"


class ProtocolError(OSError):
    """连接、帧格式或握手失败。控制面据此返回“插件服务暂不可用”。"""


class RpcFailure(Exception):
    """插件返回的错误响应，保留协议错误码（例如 not_supported、invalid_params）。"""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def _remaining(deadline: float) -> float:
    left = deadline - time.monotonic()
    if left <= 0:
        raise TimeoutError("内核协议调用超时")
    return left


def _read_exact(sock: socket.socket, count: int, deadline: float) -> bytes:
    chunks = []
    while count:
        sock.settimeout(_remaining(deadline))
        chunk = sock.recv(count)
        if not chunk:
            raise ProtocolError("插件连接已关闭")
        chunks.append(chunk)
        count -= len(chunk)
    return b"".join(chunks)


def write_frame(sock: socket.socket, message: dict, deadline: float) -> None:
    body = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(body) > MAX_FRAME_BYTES:
        raise ProtocolError("协议帧超过上限")
    sock.settimeout(_remaining(deadline))
    sock.sendall(struct.pack(">I", len(body)) + body)


def read_frame(sock: socket.socket, deadline: float) -> object:
    # 超限长度在分配正文缓冲区前拒绝，与 kernel-protocol 的 read_frame 行为一致。
    (length,) = struct.unpack(">I", _read_exact(sock, 4, deadline))
    if length > MAX_FRAME_BYTES:
        raise ProtocolError("协议帧超过上限")
    try:
        return json.loads(_read_exact(sock, length, deadline).decode("utf-8"))
    except ValueError as error:
        raise ProtocolError("协议帧内容无效") from error


class AdminConnection:
    """一条管理连接：连接后立即以 hello.role = admin 握手，之后可多次 request。"""

    def __init__(self, path: str, timeout: float, expected_id: str | None = None):
        self.deadline = time.monotonic() + timeout
        self.next_id = 1
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.settimeout(_remaining(self.deadline))
            self.sock.connect(str(path))
            self._hello(expected_id)
        except BaseException:
            self.close()
            raise

    def __enter__(self) -> "AdminConnection":
        return self

    def __exit__(self, *_) -> None:
        self.close()

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    def _hello(self, expected_id: str | None) -> None:
        result = self.request("hello", {
            "kernelApi": {"major": KERNEL_API_MAJOR, "minor": KERNEL_API_MINOR},
            "kernel": {"version": KERNEL_VERSION, "slot": None},
            "role": "admin",
        })
        if not isinstance(result, dict):
            raise ProtocolError("握手结果无效")
        api = result.get("kernelApi")
        # 与 kernel-protocol 的 is_compatible 一致：插件要求的次版本不能高于内核；主版本 0 时不接受 0.4 以下。
        min_minor = api.get("minMinor") if isinstance(api, dict) else None
        if (not isinstance(api, dict) or api.get("major") != KERNEL_API_MAJOR or type(min_minor) is not int
                or min_minor > KERNEL_API_MINOR or min_minor < KERNEL_API_MIN_MINOR):
            raise ProtocolError("插件接口版本与内核不兼容")
        if expected_id is not None and (result.get("plugin") or {}).get("id") != expected_id:
            raise ProtocolError("插件身份与注册不一致")

    def request(self, method: str, params: dict) -> object:
        request_id = self.next_id
        self.next_id += 1
        write_frame(self.sock, {"kind": "request", "id": request_id, "method": method, "params": params}, self.deadline)
        while True:
            message = read_frame(self.sock, self.deadline)
            if not isinstance(message, dict) or message.get("kind") != "response" or message.get("id") != request_id:
                continue
            if "error" in message:
                error = message["error"]
                if not isinstance(error, dict):
                    raise ProtocolError("错误响应格式无效")
                raise RpcFailure(str(error.get("code", "")), str(error.get("message", "")))
            if "result" in message:
                return message["result"]
            raise ProtocolError("响应格式无效")


def health(path: str, timeout: float, expected_id: str | None = None) -> dict:
    """管理连接握手后调用 health，返回插件的健康结果（含 ready）。"""
    with AdminConnection(path, timeout, expected_id) as connection:
        result = connection.request("health", {})
    if not isinstance(result, dict):
        raise ProtocolError("健康结果无效")
    return result


def admin_call(path: str, method: str, params: dict, role: str, timeout: float,
               expected_id: str | None = None) -> object:
    """经管理连接调用插件的 admin.call，返回插件结果原样。

    expected_id 用于把 root 注册表中的插件身份绑定到握手身份，避免 socket
    被替换后把管理请求转给另一个服务。
    """
    with AdminConnection(path, timeout, expected_id) as connection:
        return connection.request("admin.call", {"method": method, "params": params, "actor": {"role": role}})

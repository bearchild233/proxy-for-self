import json
import os
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import threading
import time
import unittest

import kernel_protocol
from kernel_protocol import MAX_FRAME_BYTES, ProtocolError, RpcFailure, admin_call, health, read_frame, write_frame

# 互通测试需要已构建的 example-echo（Linux 二进制），由环境变量提供路径；否则跳过。
EXAMPLE_ECHO = os.environ.get("CPR_EXAMPLE_ECHO_BIN", "")


def deadline(seconds=5):
    return time.monotonic() + seconds


class FramingTests(unittest.TestCase):
    def test_round_trip_matches_length_prefixed_json(self):
        left, right = socket.socketpair()
        self.addCleanup(left.close)
        self.addCleanup(right.close)
        write_frame(left, {"kind": "request", "id": 1, "method": "health", "params": {}}, deadline())
        header = right.recv(4)
        length = struct.unpack(">I", header)[0]
        body = right.recv(length)
        self.assertEqual(json.loads(body), {"kind": "request", "id": 1, "method": "health", "params": {}})

    def test_oversized_incoming_frame_is_rejected_before_reading_body(self):
        left, right = socket.socketpair()
        self.addCleanup(left.close)
        self.addCleanup(right.close)
        left.sendall(struct.pack(">I", MAX_FRAME_BYTES + 1))
        with self.assertRaises(ProtocolError):
            read_frame(right, deadline())

    def test_oversized_outgoing_frame_is_rejected(self):
        left, right = socket.socketpair()
        self.addCleanup(left.close)
        self.addCleanup(right.close)
        with self.assertRaises(ProtocolError):
            write_frame(left, {"value": "x" * MAX_FRAME_BYTES}, deadline())

    def test_invalid_json_and_truncated_frames_are_protocol_errors(self):
        left, right = socket.socketpair()
        self.addCleanup(right.close)
        left.sendall(struct.pack(">I", 3) + b"{x}")
        with self.assertRaises(ProtocolError):
            read_frame(right, deadline())
        left.close()
        left, right = socket.socketpair()
        self.addCleanup(right.close)
        left.sendall(struct.pack(">I", 10) + b"abc")
        left.close()
        with self.assertRaises(ProtocolError):
            read_frame(right, deadline())


class FakePlugin:
    """在 Unix socket 上模拟插件管理面：握手、health 与 admin.call。"""

    def __init__(self, directory, plugin_id="example-echo", ready=True, minor=5):
        self.path = str(Path(directory) / "fake.sock")
        self.plugin_id = plugin_id
        self.ready = ready
        self.minor = minor
        self.roles = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(4)
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while True:
            try:
                connection, _ = self.server.accept()
            except OSError:
                return
            with connection:
                self.handle(connection)

    def reply(self, connection, request_id, result=None, error=None):
        payload = {"kind": "response", "id": request_id}
        payload.update({"error": error} if error else {"result": result})
        write_frame(connection, payload, deadline())

    def handle(self, connection):
        while True:
            try:
                message = read_frame(connection, deadline(10))
            except (ProtocolError, OSError):
                return
            if message.get("kind") != "request":
                continue
            method, params, request_id = message["method"], message["params"], message["id"]
            if method == "hello":
                self.roles.append(params["role"])
                self.reply(connection, request_id, {
                    "plugin": {"id": self.plugin_id, "version": "test"}, "sdk": {"version": "test"},
                    "kernelApi": {"major": 0, "minMinor": self.minor}, "hooks": [], "capabilities": ["log"],
                })
            elif method == "health":
                self.reply(connection, request_id, {"ready": self.ready})
            elif method == "admin.call" and params["method"] == "echo":
                self.reply(connection, request_id, params["params"])
            elif method == "admin.call":
                self.reply(connection, request_id, error={"code": "not_supported", "message": params["method"]})
            else:
                self.reply(connection, request_id, error={"code": "forbidden", "message": method})

    def close(self):
        self.server.close()


class AdminClientTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.mkdtemp(prefix="kp-")
        self.addCleanup(shutil.rmtree, self.directory, True)
        self.fake = FakePlugin(self.directory)
        self.addCleanup(self.fake.close)

    def test_health_uses_admin_role_and_checks_identity(self):
        self.assertEqual(health(self.fake.path, 5, "example-echo"), {"ready": True})
        self.assertEqual(self.fake.roles, ["admin"])
        with self.assertRaises(ProtocolError):
            health(self.fake.path, 5, "other-plugin")

    def test_admin_call_returns_plugin_result_unchanged(self):
        value = admin_call(self.fake.path, "echo", {"nested": [1, None, "x"]}, "admin", 5)
        self.assertEqual(value, {"nested": [1, None, "x"]})

    def test_plugin_error_keeps_protocol_code(self):
        with self.assertRaises(RpcFailure) as context:
            admin_call(self.fake.path, "missing", {}, "admin", 5)
        self.assertEqual(context.exception.code, "not_supported")

    def test_incompatible_plugin_api_is_refused(self):
        other = Path(tempfile.mkdtemp(prefix="kp-old-"))
        self.addCleanup(shutil.rmtree, other, True)
        old = FakePlugin(other, minor=3)
        self.addCleanup(old.close)
        with self.assertRaises(ProtocolError):
            health(old.path, 5)

    def test_missing_socket_is_connection_error(self):
        with self.assertRaises(OSError):
            health(str(Path(self.directory) / "absent.sock"), 1)


@unittest.skipUnless(EXAMPLE_ECHO and Path(EXAMPLE_ECHO).is_file(), "需要 CPR_EXAMPLE_ECHO_BIN 指向已构建的 example-echo")
class RustInteropTests(unittest.TestCase):
    """Python 管理连接与 Rust SDK 的真实进程互通。"""

    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix="kpi-"))
        self.addCleanup(shutil.rmtree, self.directory, True)
        self.socket = self.directory / "echo.sock"
        environment = dict(os.environ, CPR_PLUGIN_SOCKET=str(self.socket), CPR_PLUGIN_DATA=str(self.directory / "data"),
                           CPR_PLUGIN_SHUTDOWN_MS="3000")
        self.process = subprocess.Popen([EXAMPLE_ECHO], env=environment)
        self.addCleanup(self.stop)
        end = time.monotonic() + 5
        while not self.socket.exists() and time.monotonic() < end:
            time.sleep(0.02)
        self.assertTrue(self.socket.exists())

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            self.process.wait(timeout=10)

    def test_health_and_admin_echo_round_trip(self):
        self.assertTrue(health(str(self.socket), 5, "example-echo")["ready"])
        self.assertEqual(admin_call(str(self.socket), "echo", {"a": [1, "b"]}, "admin", 5), {"a": [1, "b"]})

    def test_admin_connection_cannot_run_kernel_hooks(self):
        with self.assertRaises(RpcFailure) as context:
            with kernel_protocol.AdminConnection(str(self.socket), 5) as connection:
                connection.request("route.prepare", {})
        self.assertEqual(context.exception.code, "forbidden")

    def test_sigterm_exits_the_process(self):
        self.stop()
        self.assertEqual(self.process.returncode, 0)


if __name__ == "__main__":
    unittest.main()

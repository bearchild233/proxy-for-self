import fcntl
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from http.server import BaseHTTPRequestHandler
import socketserver
import threading
from unittest.mock import Mock

from plugin_registry import PluginRegistry
from plugin_manifest import resource, validate


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.registry_file = self.root / "registry.json"
        self.entry = self.plugin("sample")
        self.registry_file.write_text(json.dumps([self.entry]))
        self.registry = PluginRegistry({"registry": str(self.registry_file)}, Mock(return_value=SimpleNamespace(stdout="")))

    def plugin(self, name):
        root = self.root / name
        root.mkdir()
        (root / "runtime").mkdir()
        (root / "runtime/page.html").write_text("<h1>Sample plugin</h1>")
        (root / "state.json").write_text(json.dumps({"enabled": True, "version": "1", "operation": {"status": "idle"}}))
        manifest = {"id": name, "protocol": 1, "version": "1", "capabilities": ["sample-capability"],
                    "pages": [{"id": "home", "title": "Home", "slot": "settings", "entry": "page.html", "roles": ["admin"]}],
                    "operations": [{"id": "get-state", "roles": ["key", "admin"]}]}
        (root / "runtime/plugin.json").write_text(json.dumps(manifest))
        return {"id": name, "root": str(root), "version": "1"}

    def request(self, kind="catalog", role="admin", **kwargs):
        return self.registry.request({"kind": kind, "principal": {"role": role, "id": "key-123"}, **kwargs})

    def test_hot_registration_requires_no_process_restart(self):
        second = self.plugin("second")
        self.registry_file.write_text(json.dumps([self.entry, second]))
        self.assertEqual({p["id"] for p in self.request()}, {"sample", "second"})

    def test_required_modules_sort_last_and_cannot_be_removed_or_downgraded(self):
        second = self.plugin("z-optional")
        self.entry["required"] = True
        self.registry_file.write_text(json.dumps([self.entry, second]))
        self.assertEqual([p["id"] for p in self.request()], ["z-optional", "sample"])
        self.assertEqual([p["id"] for p in self.registry.status()], ["z-optional", "sample"])
        for entries in [[second], [{**self.entry, "required": False}, second]]:
            self.registry_file.write_text(json.dumps(entries))
            with self.assertRaisesRegex(ValueError, "基础模块不可"):
                self.registry.status()

    def test_required_is_host_policy_not_a_plugin_manifest_claim(self):
        path = self.root / "sample/runtime/plugin.json"
        manifest = json.loads(path.read_text())
        manifest["required"] = True
        path.write_text(json.dumps(manifest))
        self.assertFalse(self.request()[0]["required"])

    def test_catalog_exposes_details_without_private_manifest_fields(self):
        path = self.root / "sample/runtime/plugin.json"
        manifest = json.loads(path.read_text())
        manifest.update(category="management", details={
            "overview": "Account management", "features": ["Edit accounts"],
            "disableEffect": "Hide the page; inference continues",
        })
        manifest["pages"][0]["slot"] = "account-menu"
        path.write_text(json.dumps(manifest))
        plugin = self.request()[0]
        self.assertEqual(plugin["category"], "management")
        self.assertEqual(plugin["details"], manifest["details"])
        self.assertEqual(plugin["pages"][0]["slot"], "account-menu")
        self.assertNotIn("entry", plugin["pages"][0])
        self.assertNotIn("operations", plugin)
        self.assertEqual(self.request(role="key")[0]["pages"], [])

    def test_new_management_slots_reject_key_access(self):
        root = self.root / "sample/runtime"
        manifest = json.loads((root / "plugin.json").read_text())
        for slot in ["navigation", "account-menu", "account-detail", "group-menu", "key-menu", "proxy-menu", "usage-detail", "dashboard"]:
            with self.subTest(slot=slot):
                manifest["pages"][0].update(slot=slot, roles=["admin", "key"])
                with self.assertRaisesRegex(ValueError, "仅限管理员"):
                    validate(manifest, "sample", root)
                manifest["pages"][0]["roles"] = ["admin"]
                validate(manifest, "sample", root)

    def test_invalid_details_fail_closed_and_old_manifest_still_loads(self):
        root = self.root / "sample/runtime"
        path = root / "plugin.json"
        original = json.loads(path.read_text())
        self.assertEqual(self.request()[0]["category"], "extensions")
        for invalid in [{"category": "unknown"}, {"details": {"features": "invalid"}},
                        {"details": {"overview": "x" * 2001}},
                        {"details": {"html": "<script>"}}]:
            with self.subTest(invalid=list(invalid)):
                path.write_text(json.dumps({**original, **invalid}))
                self.assertEqual(self.request(), [])
                with self.assertRaises(ValueError):
                    self.request("page", pluginId="sample", name="home")

    def test_disabled_plugin_disappears_and_rejects_stale_page_and_rpc(self):
        self.assertEqual(len(self.request()), 1)
        self.registry.begin("sample", "disable")
        self.assertEqual(self.request(), [])
        for kind, name in [("page", "home"), ("rpc", "get-state")]:
            with self.assertRaisesRegex(ValueError, "未启用"):
                self.request(kind, pluginId="sample", name=name)
        self.assertEqual(len(self.registry.status()), 1)

    def test_role_scoped_catalog_and_direct_page_access(self):
        self.assertEqual(self.request(role="key")[0]["pages"], [])
        with self.assertRaisesRegex(ValueError, "不可访问"):
            self.request("page", "key", pluginId="sample", name="home")
        self.assertIn("Sample plugin", self.request("page", pluginId="sample", name="home")["html"])

    def test_rpc_only_declared_operation_with_verified_identity(self):
        self.registry.rpc = Mock(return_value={"ok": True})
        self.assertEqual(self.request("rpc", "key", pluginId="sample", name="get-state", input={"id": "untrusted"}), {"ok": True})
        self.assertEqual(self.registry.rpc.call_args.args[2], {"role": "key", "id": "key-123"})
        with self.assertRaises(ValueError):
            self.request("rpc", pluginId="sample", name="undeclared")
        self.assertEqual(self.registry.rpc.call_count, 1)

    def test_activity_lock_blocks_new_calls_during_swap(self):
        with (self.root / "sample/activity.lock").open("rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaisesRegex(ValueError, "正在执行"):
                self.request("page", pluginId="sample", name="home")

    def test_manifest_and_resources_fail_closed(self):
        root = self.root / "sample/runtime"
        for name in ["../state.json", "/etc/passwd", "page.html/../plugin.json"]:
            with self.assertRaises(ValueError):
                resource(root, name)
        manifest = json.loads((root / "plugin.json").read_text())
        manifest["pages"][0]["roles"] = ["public"]
        with self.assertRaises(ValueError):
            validate(manifest, "sample", root)
        manifest["pages"][0]["roles"] = ["admin"]
        (root / "page.html").unlink()
        (root / "page.html").symlink_to(self.root / "registry.json")
        with self.assertRaises(ValueError):
            validate(manifest, "sample", root)

    def test_active_plugin_cannot_be_removed_from_registry(self):
        self.registry_file.write_text("[]")
        with self.assertRaisesRegex(ValueError, "先卸载"):
            self.registry.status()

    def test_device_entry_cannot_invoke_admin_or_key_operations(self):
        with self.assertRaises(ValueError):
            self.request("rpc", "device", pluginId="sample", name="get-state")
        with self.assertRaises(ValueError):
            self.request("catalog", "device")

    def test_rpc_uses_private_socket_and_bounded_json_envelope(self):
        received = []
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                body = b'{"result":42}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            def log_message(self, *_):
                pass
        socket_path = str(self.root / "worker.sock")
        self.registry.managers["sample"].socket_path = socket_path
        with socketserver.UnixStreamServer(socket_path, Handler) as server:
            server.timeout = 2
            worker = threading.Thread(target=server.handle_request)
            worker.start()
            try:
                result = self.request("rpc", "key", pluginId="sample", name="get-state", input={"value": 7})
            finally:
                worker.join(3)
        self.assertEqual(result, {"result": 42})
        self.assertEqual(received, [("/plugins/rpc/get-state", {"principal": {"role": "key", "id": "key-123"}, "input": {"value": 7}})])

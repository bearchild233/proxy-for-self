import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import kernel_protocol
from service_plugins import ServicePlugins, instance_name

ID = "example-echo"


def elf(salt: bytes, machine=62, e_type=3) -> bytes:
    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4], header[5], header[6] = 2, 1, 1
    header[16:18] = e_type.to_bytes(2, "little")
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header) + salt


class FakeClock:
    def __init__(self):
        self.now = 0.0
        self.sleeps = []

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds


class FakeSystemd:
    def __init__(self):
        self.calls = []
        self.active = set()
        self.fail_start = False

    def __call__(self, args, timeout=60, **_):
        self.calls.append(list(args))
        if args[:2] == ["systemctl", "start"]:
            if self.fail_start:
                raise RuntimeError("系统命令执行失败：systemctl")
            self.active.add(args[2])
        elif args[:2] == ["systemctl", "stop"]:
            self.active.discard(args[2])
        elif args[:2] == ["systemctl", "is-active"]:
            if args[2] not in self.active:
                raise RuntimeError("系统命令执行失败：systemctl")
            return SimpleNamespace(stdout="active\n")
        return SimpleNamespace(stdout="")

    def started(self):
        return [call[2] for call in self.calls if call[:2] == ["systemctl", "start"]]

    def stopped(self):
        return [call[2] for call in self.calls if call[:2] == ["systemctl", "stop"]]


class FakeHealth:
    def __init__(self):
        self.ready = True
        self.calls = []

    def __call__(self, path, timeout, expected_id=None):
        self.calls.append((path, expected_id))
        if not self.ready:
            raise kernel_protocol.ProtocolError("插件连接已关闭")
        return {"ready": True}


class ServiceTests(unittest.TestCase):
    def setUp(self):
        self.temp = Path(tempfile.mkdtemp(prefix="svc-"))
        self.addCleanup(shutil.rmtree, self.temp, True)
        self.root = self.temp
        self.systemd = FakeSystemd()
        self.health = FakeHealth()
        self.clock = FakeClock()
        self.config = {
            "registry_file": str(self.root / "etc/proxy-for-self/plugins/registry.json"),
            "store_root": str(self.root / "store"),
            "runtime_root": str(self.root / "runtime"),
            "socket_root": str(self.root / "sock"),
            "data_root": str(self.root / "data"),
            "env_directory": str(self.root / "etc/proxy-for-self/plugins/services"),
            "unit_directory": str(self.root / "units"),
            "state_file": str(self.root / "state/service-plugins.json"),
            "user": "proxy-for-self-plugins",
            "group": "proxy-for-self-plugins",
            "allow": {ID: {"grants": ["log"], "fetchHosts": ["pool.example.test"]}},
        }
        self.counter = 0

    def manager(self, **overrides):
        options = dict(run=self.systemd, health=self.health, identity=lambda user, group: (os.getuid(), os.getgid()),
                       sleep=self.clock.sleep, clock=self.clock)
        options.update(overrides)
        return ServicePlugins(self.config, options.pop("run"), **options)

    def make_version(self, capabilities=("log",), hosts=(), binary=None, service=None):
        """在存储中写入一个版本包（结构与控制面 StaticPluginStore 一致）。"""
        self.counter += 1
        content = binary if binary is not None else elf(b"salt-%d" % self.counter)
        manifest = {
            "id": ID, "version": "0.1.%d" % self.counter, "protocol": 2, "kind": "ui", "pages": [],
            "capabilities": ["example-echo.echo"],
            "resources": {"service/example-echo": hashlib.sha256(content).hexdigest()},
            "service": {"binary": "service/example-echo", "kernelApi": {"major": 0, "minMinor": 5},
                        "capabilities": list(capabilities), "fetchHosts": list(hosts),
                        "memoryMax": "64M", "shutdownSeconds": 30, **(service or {})},
        }
        text = json.dumps(manifest, sort_keys=True).encode()
        digest = hashlib.sha256(content + text).hexdigest()
        directory = self.root / "store" / ID / "releases" / digest
        (directory / "service").mkdir(parents=True)
        (directory / "service/example-echo").write_bytes(content)
        (directory / "plugin.json").write_bytes(text)
        return digest

    def run_operation(self, manager, action, version=None):
        operation_id = manager.begin(action, ID, version)
        manager.execute(operation_id, ID, action, version)
        return manager.state["operation"]

    def registry(self):
        return json.loads((self.root / "etc/proxy-for-self/plugins/registry.json").read_text())["plugins"][0]

    def runtime_dirs(self):
        return sorted(item.name for item in (self.root / "runtime").iterdir())

    # ----- 激活与注册表 -----

    def test_activate_writes_instance_files_registry_and_stops_nothing_on_first_install(self):
        manager = self.manager()
        version = self.make_version(capabilities=("log", "quota.read"), hosts=("pool.example.test", "other.test"))
        operation = self.run_operation(manager, "activate", version)
        self.assertEqual(operation["status"], "succeeded", operation)
        instance = instance_name(ID, version)
        self.assertEqual(self.systemd.started(), [f"proxy-for-self-plugin@{instance}.service"])
        self.assertEqual(self.systemd.stopped(), [])
        self.assertEqual(self.clock.sleeps, [10])
        self.assertEqual(self.registry(), {
            "id": ID, "enabled": True, "socket": str(self.root / "sock" / ID / f"{instance}.sock"),
            "uid": os.getuid(), "grants": ["log"], "fetchHosts": ["pool.example.test"],
        })
        env = (self.root / "etc/proxy-for-self/plugins/services" / f"{instance}.env").read_text().splitlines()
        self.assertIn("CPR_PLUGIN_SHUTDOWN_MS=30000", env)
        self.assertIn(f"CPR_PLUGIN_SOCKET={self.root / 'sock' / ID / (instance + '.sock')}", env)
        self.assertIn(f"PLUGIN_BINARY={self.root / 'runtime' / instance / 'service'}", env)
        dropin = (self.root / "units" / f"proxy-for-self-plugin@{instance}.service.d" / "limits.conf").read_text()
        self.assertEqual(dropin, "[Service]\nMemoryMax=64M\nTimeoutStopSec=35\n")
        copied = self.root / "runtime" / instance / "service"
        self.assertEqual(copied.stat().st_mode & 0o777, 0o755)
        self.assertEqual(copied.read_bytes(), (self.root / "store" / ID / "releases" / version / "service/example-echo").read_bytes())
        self.assertEqual((self.root / "data" / ID).stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.health.calls[-1], (str(self.root / "sock" / ID / f"{instance}.sock"), ID))
        self.assertEqual(manager.state["plugins"][ID]["current"], version)
        self.assertIsNone(manager.state["plugins"][ID]["previous"])

    def test_activate_switches_registry_then_stops_old_instance_after_wait(self):
        manager = self.manager()
        first = self.make_version()
        second = self.make_version()
        self.run_operation(manager, "activate", first)
        self.clock.sleeps.clear()
        self.systemd.calls.clear()
        self.assertEqual(self.run_operation(manager, "activate", second)["status"], "succeeded")
        old, new = instance_name(ID, first), instance_name(ID, second)
        self.assertEqual(self.systemd.started(), [f"proxy-for-self-plugin@{new}.service"])
        self.assertEqual(self.systemd.stopped(), [f"proxy-for-self-plugin@{old}.service"])
        self.assertEqual(self.clock.sleeps, [10])
        self.assertTrue(self.registry()["socket"].endswith(new + ".sock"))
        self.assertEqual(manager.state["plugins"][ID]["previous"], first)
        self.assertEqual(self.runtime_dirs(), sorted([old, new]))

    def test_only_current_and_previous_runtime_directories_are_kept(self):
        manager = self.manager()
        versions = [self.make_version() for _ in range(3)]
        for version in versions:
            self.assertEqual(self.run_operation(manager, "activate", version)["status"], "succeeded")
        self.assertEqual(self.runtime_dirs(), sorted(instance_name(ID, v) for v in versions[1:]))
        self.assertFalse((self.root / "etc/proxy-for-self/plugins/services" / f"{instance_name(ID, versions[0])}.env").exists())

    def test_health_failure_removes_new_instance_and_keeps_old_version(self):
        manager = self.manager()
        first = self.make_version()
        self.run_operation(manager, "activate", first)
        self.systemd.calls.clear()
        self.health.ready = False
        second = self.make_version()
        operation = self.run_operation(manager, "activate", second)
        self.assertEqual(operation["status"], "failed")
        self.assertIn("健康检查", operation["error"])
        new = instance_name(ID, second)
        self.assertEqual(self.systemd.stopped(), [f"proxy-for-self-plugin@{new}.service"])
        self.assertEqual(self.runtime_dirs(), [instance_name(ID, first)])
        self.assertFalse((self.root / "etc/proxy-for-self/plugins/services" / f"{new}.env").exists())
        self.assertTrue(self.registry()["socket"].endswith(instance_name(ID, first) + ".sock"))
        self.assertEqual(manager.state["plugins"][ID]["current"], first)

    def test_systemd_start_failure_leaves_no_new_files(self):
        manager = self.manager()
        self.systemd.fail_start = True
        version = self.make_version()
        operation = self.run_operation(manager, "activate", version)
        self.assertEqual(operation["status"], "failed")
        self.assertEqual(self.runtime_dirs(), [])
        self.assertFalse((self.root / "etc/proxy-for-self/plugins/registry.json").exists())

    def test_tampered_store_package_is_rejected_before_any_change(self):
        manager = self.manager()
        version = self.make_version()
        (self.root / "store" / ID / "releases" / version / "service/example-echo").write_bytes(elf(b"tampered"))
        operation = self.run_operation(manager, "activate", version)
        self.assertEqual(operation["status"], "failed")
        self.assertEqual(self.systemd.calls, [])
        self.assertFalse((self.root / "runtime").exists())

    def test_undeclared_file_in_store_package_is_rejected(self):
        manager = self.manager()
        version = self.make_version()
        (self.root / "store" / ID / "releases" / version / "extra.so").write_bytes(b"x")
        self.assertEqual(self.run_operation(manager, "activate", version)["status"], "failed")
        self.assertEqual(self.systemd.calls, [])

    def test_non_elf_or_wrong_architecture_binary_is_rejected(self):
        manager = self.manager()
        for binary in (b"#!/bin/sh\necho", elf(b"arm", machine=183)):
            with self.subTest(binary=binary[:6]):
                version = self.make_version(binary=binary)
                self.assertEqual(self.run_operation(manager, "activate", version)["status"], "failed")
        self.assertEqual(self.systemd.calls, [])

    def test_plugin_without_allow_entry_cannot_enable_service(self):
        self.config["allow"] = {}
        manager = self.manager()
        version = self.make_version()
        operation = self.run_operation(manager, "activate", version)
        self.assertEqual(operation["status"], "failed")
        self.assertIn("未登记", operation["error"])
        self.assertEqual(self.systemd.calls, [])

    def test_manifest_grants_and_hosts_are_capped_by_allow(self):
        manager = self.manager()
        version = self.make_version(capabilities=("log", "upstream.probe"), hosts=("pool.example.test", "evil.test"))
        self.assertEqual(self.run_operation(manager, "activate", version)["status"], "succeeded")
        self.assertEqual(self.registry()["grants"], ["log"])
        self.assertEqual(self.registry()["fetchHosts"], ["pool.example.test"])

    # ----- 禁用、启用、回退、卸载 -----

    def test_disable_closes_registry_waits_then_stops_and_keeps_runtime(self):
        manager = self.manager()
        version = self.make_version()
        self.run_operation(manager, "activate", version)
        self.clock.sleeps.clear()
        self.systemd.calls.clear()
        self.assertEqual(self.run_operation(manager, "disable")["status"], "succeeded")
        self.assertFalse(self.registry()["enabled"])
        self.assertEqual(self.clock.sleeps, [10])
        self.assertEqual(self.systemd.stopped(), [f"proxy-for-self-plugin@{instance_name(ID, version)}.service"])
        self.assertTrue((self.root / "runtime" / instance_name(ID, version)).is_dir())
        self.assertFalse(manager.state["plugins"][ID]["enabled"])

    def test_enable_reuses_existing_runtime_and_verifies_binary(self):
        manager = self.manager()
        version = self.make_version()
        self.run_operation(manager, "activate", version)
        self.run_operation(manager, "disable")
        self.assertEqual(self.run_operation(manager, "enable")["status"], "succeeded")
        self.assertTrue(self.registry()["enabled"])
        self.assertTrue(manager.state["plugins"][ID]["enabled"])
        runtime_binary = self.root / "runtime" / instance_name(ID, version) / "service"
        runtime_binary.write_bytes(elf(b"changed"))
        self.run_operation(manager, "disable")
        operation = self.run_operation(manager, "enable")
        self.assertEqual(operation["status"], "failed")
        self.assertIn("校验失败", operation["error"])

    def test_rollback_reactivates_previous_and_stops_current(self):
        manager = self.manager()
        first, second = self.make_version(), self.make_version()
        self.run_operation(manager, "activate", first)
        self.run_operation(manager, "activate", second)
        self.systemd.calls.clear()
        self.assertEqual(self.run_operation(manager, "rollback")["status"], "succeeded")
        self.assertTrue(self.registry()["socket"].endswith(instance_name(ID, first) + ".sock"))
        self.assertEqual(self.systemd.stopped(), [f"proxy-for-self-plugin@{instance_name(ID, second)}.service"])
        self.assertEqual(manager.state["plugins"][ID]["current"], first)
        self.assertEqual(manager.state["plugins"][ID]["previous"], second)
        self.assertEqual(self.runtime_dirs(), sorted([instance_name(ID, first), instance_name(ID, second)]))

    def test_rollback_without_previous_fails_without_side_effects(self):
        manager = self.manager()
        self.run_operation(manager, "activate", self.make_version())
        self.systemd.calls.clear()
        operation = self.run_operation(manager, "rollback")
        self.assertEqual(operation["status"], "failed")
        self.assertEqual(self.systemd.calls, [])

    def test_uninstall_removes_registry_and_instances_but_keeps_data(self):
        manager = self.manager()
        version = self.make_version()
        self.run_operation(manager, "activate", version)
        (self.root / "data" / ID / "state.db").write_text("kept")
        self.assertEqual(self.run_operation(manager, "uninstall")["status"], "succeeded")
        self.assertEqual(json.loads((self.root / "etc/proxy-for-self/plugins/registry.json").read_text())["plugins"], [])
        self.assertEqual(self.runtime_dirs(), [])
        self.assertFalse((self.root / "etc/proxy-for-self/plugins/services" / f"{instance_name(ID, version)}.env").exists())
        self.assertFalse((self.root / "units" / f"proxy-for-self-plugin@{instance_name(ID, version)}.service.d").exists())
        self.assertTrue((self.root / "data" / ID / "state.db").exists())
        self.assertNotIn(ID, manager.state["plugins"])

    # ----- 互斥、中断与状态 -----

    def test_service_operation_refuses_while_system_or_plugin_operation_runs(self):
        busy = {"value": True}
        manager = self.manager(busy=lambda: busy["value"])
        with self.assertRaisesRegex(ValueError, "正在执行"):
            manager.begin("activate", ID, self.make_version())
        busy["value"] = False
        operation_id = manager.begin("activate", ID, self.make_version())
        with self.assertRaisesRegex(ValueError, "正在执行"):
            manager.begin("disable", ID)
        self.assertTrue(manager.running)
        manager.execute(operation_id, ID, "activate", manager.state["operation"]["version"])
        self.assertFalse(manager.running)

    def test_interrupted_operation_is_reported_and_blocks_new_operations_after_restart(self):
        state_path = Path(self.config["state_file"])
        state_path.parent.mkdir(parents=True)
        value = {"plugins": {}, "recovery_required": False,
                 "operation": {"id": "service-x", "plugin": ID, "action": "activate", "status": "running", "message": "m"}}
        state_path.write_text(json.dumps(value))
        restarted = self.manager()
        status = restarted.status()
        self.assertEqual(status["operation"]["status"], "interrupted")
        self.assertTrue(status["recoveryRequired"])
        with self.assertRaisesRegex(ValueError, "中断"):
            restarted.begin("disable", ID)

    def test_status_reports_systemd_state_and_last_health(self):
        manager = self.manager()
        version = self.make_version()
        self.run_operation(manager, "activate", version)
        status = manager.status()
        plugin = status["plugins"][0]
        self.assertEqual(plugin["id"], ID)
        self.assertTrue(plugin["enabled"])
        self.assertEqual(plugin["systemd"]["current"], "active")
        self.assertEqual(plugin["health"]["ready"], True)
        self.assertEqual(status["operation"]["status"], "succeeded")

    def test_invalid_actions_and_versions_are_rejected(self):
        manager = self.manager()
        with self.assertRaises(ValueError):
            manager.begin("exec", ID)
        with self.assertRaises(ValueError):
            manager.begin("activate", ID, "../escape")
        with self.assertRaises(ValueError):
            manager.begin("activate", ID)
        with self.assertRaises(ValueError):
            manager.begin("activate", "Bad_ID", "a" * 64)

    def test_config_requires_absolute_paths_and_known_capabilities(self):
        with self.assertRaises(ValueError):
            ServicePlugins({**self.config, "runtime_root": "relative"}, self.systemd)
        with self.assertRaises(ValueError):
            ServicePlugins({**self.config, "allow": {ID: {"grants": ["exec"], "fetchHosts": []}}}, self.systemd)
        missing = dict(self.config)
        del missing["allow"]
        with self.assertRaises(ValueError):
            ServicePlugins(missing, self.systemd)


class UpdaterWiringTests(unittest.TestCase):
    """Updater 层的互斥与私有路由入口；后台执行函数被替换，不调用真实 systemd。"""

    def setUp(self):
        from updater import Updater
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        (root / "release").mkdir()
        self.config = {
            "release_root": str(root / "release"), "state_file": str(root / "state.json"),
            "initial_version": "0.1.0", "service": "fixture.service",
            "service_plugins": {
                "registry_file": str(root / "registry.json"), "store_root": str(root / "store"),
                "runtime_root": str(root / "runtime"), "socket_root": str(root / "sock"),
                "data_root": str(root / "data"), "env_directory": str(root / "env"),
                "unit_directory": str(root / "units"), "state_file": str(root / "service.json"),
                "user": "proxy-for-self-plugins", "group": "proxy-for-self-plugins",
                "allow": {ID: {"grants": ["log"], "fetchHosts": []}},
            },
        }
        self.updater = Updater(self.config)

    def test_service_operation_blocks_system_and_excel_operations_until_finished(self):
        from types import SimpleNamespace
        self.updater.plugins = SimpleNamespace(running=False)
        with patch.object(self.updater.services, "execute"):
            result = self.updater.service_submit({"id": ID, "action": "uninstall"})
        self.assertTrue(result["operation_id"].startswith("service-"))
        with self.assertRaises(ValueError):
            self.updater.service_submit({"id": ID, "action": "disable"})
        with self.assertRaises(ValueError):
            self.updater.plugin_submit("excel-bridge", "disable")
        with self.assertRaises(ValueError):
            self.updater.submit("restart", None)
        self.assertEqual(self.updater.service_status()["operation"]["status"], "running")

    def test_service_routes_reject_unknown_fields_and_unconfigured_updater(self):
        with self.assertRaises(ValueError):
            self.updater.service_submit({"id": ID, "action": "disable", "command": "rm"})
        self.updater.services = None
        with self.assertRaises(ValueError):
            self.updater.service_status()


class WritableScopeTests(unittest.TestCase):
    """写入范围收窄：服务 env 位于 /etc/proxy-for-self/plugins/ 下，内核注册表位于网关可读的 /etc/proxy-for-self-kernel/；单元不得放开整个 /etc/proxy-for-self。"""

    ROOT = Path(__file__).resolve().parents[2]

    def test_example_config_writes_registry_and_env_only_under_scoped_directories(self):
        config = json.loads((self.ROOT / "services/updater/config.example.json").read_text(encoding="utf-8"))
        services = config["service_plugins"]
        self.assertEqual(services["registry_file"], "/etc/proxy-for-self-kernel/registry.json")
        self.assertEqual(services["env_directory"], "/etc/proxy-for-self/plugins/services")

    def test_updater_unit_grants_plugins_directory_not_whole_proxy_for_self_directory(self):
        unit = (self.ROOT / "deploy/systemd/proxy-for-self-updater.service").read_text(encoding="utf-8")
        paths = [item for line in unit.splitlines() if line.startswith("ReadWritePaths=")
                 for item in line.split("=", 1)[1].split()]
        self.assertIn("-/etc/proxy-for-self/plugins", paths)
        self.assertIn("-/etc/proxy-for-self-kernel", paths)
        self.assertNotIn("-/etc/proxy-for-self", paths)

    def test_deployed_units_use_the_same_registry_and_env_locations(self):
        plugin_unit = (self.ROOT / "deploy/plugin-platform/proxy-for-self-plugin@.service").read_text(encoding="utf-8")
        control_unit = (self.ROOT / "deploy/plugin-platform/proxy-for-self-plugins.service").read_text(encoding="utf-8")
        self.assertIn("EnvironmentFile=/etc/proxy-for-self/plugins/services/%i.env", plugin_unit)
        self.assertIn("--service-registry /etc/proxy-for-self/plugins/registry.json", control_unit)


if __name__ == "__main__":
    unittest.main()

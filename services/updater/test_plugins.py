import fcntl
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

from plugins import PluginManager


class PluginTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'state.json').write_text(json.dumps({'enabled': True, 'version': '1', 'operation': {'status': 'idle'}}))
        (self.root / 'activity.lock').touch()
        (self.root / 'runtime').mkdir()
        self.commands = []
        self.dependencies = ''
        self.manager = PluginManager({'root': str(self.root), 'version': '2', 'drain_seconds': 0.5}, self.run_command)
        self.manager.check_health = Mock()

    def run_command(self, args, **kwargs):
        if 'Requires' in args:
            return SimpleNamespace(stdout=self.dependencies)
        self.commands.append(args)
        return SimpleNamespace(stdout='on-failure\n')

    def test_gateway_dependency_rejects_before_disabling_or_stopping(self):
        self.dependencies = 'api-hub-postgres.service api-hub-excel.socket'
        with self.assertRaisesRegex(ValueError, '强依赖'):
            self.manager.begin('excel-bridge', 'disable')
        self.assertTrue(self.manager.state['enabled'])
        self.assertFalse(self.commands)

    def test_required_module_rejects_disable_and_uninstall_without_side_effects(self):
        self.manager.config["required"] = True
        original = (self.root / 'state.json').read_bytes()
        for action in ['disable', 'uninstall']:
            with self.subTest(action=action), self.assertRaisesRegex(ValueError, '基础模块不可'):
                self.manager.begin('excel-bridge', action)
            self.assertEqual((self.root / 'state.json').read_bytes(), original)
            self.assertTrue(self.manager.status()['enabled'])
            self.assertTrue(self.manager.status()['required'])
            self.assertFalse(self.commands)

    def test_required_module_still_accepts_update(self):
        self.manager.config["required"] = True
        operation_id = self.manager.begin('excel-bridge', 'update')
        self.assertTrue(operation_id.startswith('plugin-'))

    def package(self, extra=None):
        archive = self.root / 'package.tar.gz'
        files = {'plugin.json': json.dumps({'id': 'excel-bridge', 'version': '2', 'protocol': 1}).encode(),
                 'venv/lib/python3.10/site-packages/excel_codex_bridge/hub_worker.py': b'# fixture'}
        with tarfile.open(archive, 'w:gz') as tar:
            for name, data in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data))
            if extra:
                tar.addfile(extra)
        self.manager.config.update(package=str(archive), sha256=hashlib.sha256(archive.read_bytes()).hexdigest())

    def test_disable_waits_for_active_stream_before_stopping_service(self):
        with (self.root / 'activity.lock').open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_SH)
            self.manager.begin('excel-bridge', 'disable')
            self.assertFalse(json.loads((self.root / 'state.json').read_text())['enabled'])
            worker = threading.Thread(target=self.manager.execute, args=('disable',))
            worker.start()
            time.sleep(0.1)
            self.assertFalse(self.commands)
        worker.join(2)
        self.assertFalse(worker.is_alive())
        self.assertEqual(self.manager.status()['operation']['status'], 'succeeded')
        self.assertTrue(self.manager.status()['installed'])
        self.assertFalse(self.manager.status()['enabled'])

    def test_timeout_never_stops_or_removes_active_runtime(self):
        self.manager.config['drain_seconds'] = 0
        with (self.root / 'activity.lock').open('rb') as lock:
            fcntl.flock(lock, fcntl.LOCK_SH)
            self.manager.begin('excel-bridge', 'uninstall')
            self.manager.execute('uninstall')
        self.assertFalse(self.commands)
        self.assertTrue(self.manager.status()['installed'])
        self.assertEqual(self.manager.status()['operation']['status'], 'failed')

    def test_uninstall_and_reinstall_preserve_control_state(self):
        self.package()
        self.manager.begin('excel-bridge', 'uninstall')
        self.manager.execute('uninstall')
        self.assertFalse(self.manager.status()['installed'])
        self.assertTrue((self.root / 'activity.lock').exists())
        self.manager.begin('excel-bridge', 'install')
        self.manager.execute('install')
        self.assertEqual(self.manager.status()['operation']['status'], 'succeeded')
        self.assertTrue(self.manager.status()['enabled'])
        self.assertEqual(self.manager.status()['version'], '2')

    def test_invalid_archive_restores_previous_runtime_and_enabled_state(self):
        member = tarfile.TarInfo('../escape')
        self.package(member)
        self.manager.begin('excel-bridge', 'update')
        self.manager.execute('update')
        self.assertEqual(self.manager.status()['operation']['status'], 'failed')
        self.assertTrue(self.manager.status()['installed'])
        self.assertTrue(self.manager.status()['enabled'])

    def test_failed_worker_health_never_enables_plugin(self):
        self.package()
        self.manager.check_health.side_effect = ValueError('Worker unavailable')
        self.manager.begin('excel-bridge', 'update')
        self.manager.execute('update')
        self.assertFalse(self.manager.status()['enabled'])
        self.assertEqual(self.manager.status()['operation']['status'], 'failed')

    def test_refuses_runtime_symlink(self):
        (self.root / 'runtime').rmdir()
        (self.root / 'runtime').symlink_to(self.root.parent)
        with self.assertRaises(ValueError):
            self.manager.remove_runtime()

    def test_interrupted_operation_is_disabled_and_recoverable(self):
        self.manager.begin('excel-bridge', 'uninstall')
        recovered = PluginManager(self.manager.config, self.run_command)
        self.assertFalse(recovered.status()['enabled'])
        self.assertEqual(recovered.status()['operation']['status'], 'failed')
        recovered.begin('excel-bridge', 'disable')
        with self.assertRaises(ValueError):
            recovered.begin('excel-bridge', 'enable')

    def test_failed_update_restores_old_runtime_without_leaving_backup(self):
        old = self.root / "runtime"
        (old / "marker").write_text("old-runtime")
        (old / "plugin.json").write_text(json.dumps({"id": "excel-bridge", "version": "1", "protocol": 1}))
        self.package()
        self.manager.check_health.side_effect = ValueError("unhealthy")
        self.manager.begin("excel-bridge", "update")
        self.manager.execute("update")
        self.assertEqual((old / "marker").read_text(), "old-runtime")
        self.assertEqual(self.manager.status()["version"], "1")
        self.assertFalse((self.root / ".previous-runtime").exists())
        self.assertEqual(self.manager.status()["operation"]["status"], "failed")

    def test_interrupted_swap_recovers_old_runtime_on_disable(self):
        previous = self.root / ".previous-runtime"
        previous.mkdir()
        (previous / "marker").write_text("old-runtime")
        self.manager.begin("excel-bridge", "update")
        recovered = PluginManager(self.manager.config, self.run_command)
        recovered.begin("excel-bridge", "disable")
        recovered.execute("disable")
        self.assertEqual((self.root / "runtime/marker").read_text(), "old-runtime")
        self.assertFalse(previous.exists())
        self.assertFalse(recovered.status()["enabled"])

    def test_candidate_failure_restores_healthy_enabled_previous_worker(self):
        old=self.root/'runtime'
        (old/'plugin.json').write_text(json.dumps({'id':'excel-bridge','version':'1','protocol':1}))
        (old/'marker').write_text('previous')
        self.package()
        self.manager.check_health.side_effect=[ValueError('candidate unhealthy'),None]
        self.manager.begin('excel-bridge','update');self.manager.execute('update')
        self.assertTrue(self.manager.status()['enabled'])
        self.assertEqual(self.manager.status()['version'],'1')
        self.assertEqual(self.manager.check_health.call_count,2)
        self.assertFalse((self.root/'.previous-runtime').exists())

    def test_disabled_plugin_update_stays_disabled(self):
        self.manager.state['enabled']=False;self.manager.save();self.package()
        self.manager.begin('excel-bridge','update');self.manager.execute('update')
        self.assertFalse(self.manager.status()['enabled'])
        self.assertEqual(self.manager.status()['version'],'2')
        self.manager.check_health.assert_not_called()

    def test_interrupted_valid_update_restores_enabled_previous_worker(self):
        from unittest.mock import patch
        old=self.root/'runtime'
        (old/'plugin.json').write_text(json.dumps({'id':'excel-bridge','version':'1','protocol':1}))
        self.manager.begin('excel-bridge','update')
        with patch.object(PluginManager,'check_health') as health:
            recovered=PluginManager(self.manager.config,self.run_command)
        self.assertTrue(recovered.status()['enabled'])
        self.assertEqual(recovered.status()['operation']['status'],'succeeded')
        health.assert_called_once()

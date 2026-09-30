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

    def test_invalid_archive_keeps_previous_runtime_disabled(self):
        member = tarfile.TarInfo('../escape')
        self.package(member)
        self.manager.begin('excel-bridge', 'update')
        self.manager.execute('update')
        self.assertEqual(self.manager.status()['operation']['status'], 'failed')
        self.assertTrue(self.manager.status()['installed'])
        self.assertFalse(self.manager.status()['enabled'])

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

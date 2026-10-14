import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import signal
import unittest
from unittest.mock import Mock, patch

from server import Platform, package_directory, REPO
from lifecycle import Lifecycle

import sys
sys.path.insert(0, str(REPO / 'services/updater'))
from test_kernel_protocol import FakePlugin  # noqa: E402

ID = 'example-echo'
EXAMPLE_ECHO = os.environ.get('CPR_EXAMPLE_ECHO_BIN', '')
SERVICE = {'binary': 'service/example-echo', 'kernelApi': {'major': 0, 'minMinor': 5}, 'capabilities': ['log'],
           'fetchHosts': [], 'memoryMax': '64M', 'shutdownSeconds': 30}


def elf(salt):
    header = bytearray(64)
    header[0:4] = b'\x7fELF'
    header[4], header[5], header[6] = 2, 1, 1
    header[16:18] = (3).to_bytes(2, 'little')
    header[18:20] = (62).to_bytes(2, 'little')
    return bytes(header) + salt


class FakeUpdater:
    """替代 updater 私有 socket：记录请求，并按配置让指定操作失败。"""

    def __init__(self):
        self.requests = []
        self.failing = set()
        self.operation = {'status': 'idle'}

    def __call__(self, method, path, value=None):
        self.requests.append((method, path, value))
        if method == 'POST':
            operation_id = 'op-%d' % len(self.requests)
            failed = value['action'] in self.failing
            self.operation = {'id': operation_id, 'status': 'failed' if failed else 'succeeded',
                              'message': '健康检查未通过' if failed else ''}
            return {'operation_id': operation_id}
        return {'operation': self.operation}

    def posted(self):
        return [(value['action'], value.get('version')) for method, _, value in self.requests if method == 'POST']


class ServicePlatformTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.registry = self.root / 'plugin-registry.json'
        self.platform = Platform(self.root / 'store', 18336, None, None, str(self.registry))
        self.platform.register({'id': ID, 'name': 'Echo', 'description': 'Test', 'capabilities': [ID + '.echo'], 'service': SERVICE})
        self.updater = FakeUpdater()
        self.platform.lifecycle.updater = self.updater
        self.platform.lifecycle.updater_socket = 'fake-updater.sock'
        self.counter = 0

    def package(self, version='1'):
        self.counter += 1
        directory = self.root / ('pkg-%s-%d' % (version, self.counter))
        (directory / 'service').mkdir(parents=True)
        binary = elf(('salt-%d' % self.counter).encode())
        (directory / 'service/example-echo').write_bytes(binary)
        manifest = {'id': ID, 'version': version, 'protocol': 2, 'kind': 'ui', 'pages': [],
                    'capabilities': [ID + '.echo'], 'resources': {'service/example-echo': hashlib.sha256(binary).hexdigest()},
                    'service': SERVICE}
        (directory / 'plugin.json').write_text(json.dumps(manifest))
        archive = self.root / ('pkg-%s-%d.tar.gz' % (version, self.counter))
        package_directory(directory, archive)
        return archive, hashlib.sha256(archive.read_bytes()).hexdigest()

    def install(self, version):
        return self.platform.store.install(ID, *self.package(version))['current']

    def fake_plugin(self):
        directory = Path(tempfile.mkdtemp(prefix='fp-'))
        self.addCleanup(lambda: shutil.rmtree(directory, True))
        plugin = FakePlugin(directory)
        self.addCleanup(plugin.close)
        return plugin

    def wait(self):
        for _ in range(500):
            if self.platform.lifecycle.operations[ID]['status'] != 'running':
                return self.platform.lifecycle.operations[ID]
            time.sleep(0.01)
        self.fail('service operation did not finish')

    def write_registry(self, socket_path, enabled=True):
        self.registry.write_text(json.dumps({'plugins': [{'id': ID, 'enabled': enabled, 'socket': str(socket_path),
                                                           'uid': os.getuid(), 'grants': ['log'], 'fetchHosts': []}]}))

    # ----- 注册与能力声明 -----

    def test_service_capability_needs_service_section_and_own_prefix(self):
        with self.assertRaises(ValueError):
            self.platform.register({'id': 'sample', 'name': 'S', 'description': 'T', 'capabilities': ['sample.echo']})
        with self.assertRaises(ValueError):
            self.platform.register({'id': 'sample', 'name': 'S', 'description': 'T', 'capabilities': ['other.echo'], 'service': SERVICE})
        policy = self.platform.register({'id': 'sample', 'name': 'S', 'description': 'T', 'capabilities': ['sample.echo.deep'], 'service': SERVICE})
        self.assertEqual(policy['capabilities'], ['sample.echo.deep'])

    # ----- 管理调用转发 -----

    def test_gateway_pool_uses_current_service_manifest_and_falls_back_on_rollback(self):
        service = json.loads((REPO / 'plugins/gateway-pool/plugin.json').read_text(encoding='utf-8'))
        legacy = {key: value for key, value in service.items() if key != 'service'}
        calls = Mock()
        calls.call.return_value = {'source': 'service'}
        self.platform.service_calls = calls
        # 页面可以仍租用升级前的包；选择后端必须跟随当前安装版本，包括回退。
        for current, page in [(service, legacy), (legacy, service)]:
            with self.subTest(service='service' in current), \
                    patch.object(self.platform.store, 'state', return_value={'enabled': True}), \
                    patch.object(self.platform.store, 'manifest', side_effect=lambda _, version=None: page if version else current), \
                    patch.object(self.platform.gateway_pool, 'configure', return_value={'source': 'python'}) as configure, \
                    patch.object(self.platform.gateway_pool, 'update_queue', return_value={'source': 'python'}) as queue, \
                    patch.object(self.platform.gateway_pool, 'view', return_value={'source': 'python'}) as view, \
                    patch.object(self.platform.gateway_pool, 'settings_for_ui', return_value={}), \
                    patch.object(self.platform.gateway_pool, 'statistics', return_value={'source': 'python'}) as history:
                for method, target in [('configure', configure), ('queue.update', queue), ('read', view), ('history.read', history)]:
                    calls.reset_mock()
                    target.reset_mock()
                    data = {'nested': [1, None]}
                    status, body, _ = self.platform.call('gateway-pool', 'page', 'gateway-pool.' + method, data, 'session', 'admin', 'http://localhost')
                    self.assertEqual(status, 200)
                    self.assertEqual(json.loads(body)['source'], 'service' if 'service' in current else 'python')
                    if 'service' in current:
                        calls.call.assert_called_once_with('gateway-pool', method, data, 'admin')
                        target.assert_not_called()
                    else:
                        calls.call.assert_not_called()
                        target.assert_called_once()
                    calls.reset_mock()
                    target.reset_mock()
                    for role in ('key', '', 'viewer'):
                        with self.assertRaises(PermissionError):
                            self.platform.call('gateway-pool', 'page', 'gateway-pool.' + method, {}, 'session', role, 'http://localhost')
                    calls.call.assert_not_called()
                    target.assert_not_called()

    def test_admin_call_is_forwarded_to_registry_socket_and_restricted_to_admin(self):
        version = self.install('1')
        plugin = self.fake_plugin()
        self.write_registry(plugin.path)
        status, body, _ = self.platform.call(ID, version, ID + '.echo', {'nested': [1, None]}, 'session', 'admin', 'http://localhost')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'nested': [1, None]})
        self.assertEqual(plugin.roles, ['admin'])
        with self.assertRaises(PermissionError):
            self.platform.call(ID, version, ID + '.echo', {}, 'session', 'key', 'http://localhost')

    def test_disabled_or_missing_registry_entry_is_unavailable(self):
        version = self.install('1')
        self.write_registry('/run/none.sock', enabled=False)
        with self.assertRaises(ConnectionError):
            self.platform.call(ID, version, ID + '.echo', {}, 'session', 'admin', 'http://localhost')
        self.registry.write_text(json.dumps({'plugins': []}))
        with self.assertRaises(ConnectionError):
            self.platform.call(ID, version, ID + '.echo', {}, 'session', 'admin', 'http://localhost')

    def test_plugin_method_not_implemented_is_forbidden_not_500(self):
        plugin = self.fake_plugin()
        self.write_registry(plugin.path)
        with self.assertRaises(PermissionError):
            self.platform.service_calls.call(ID, 'missing', {}, 'admin')

    @unittest.skipUnless(EXAMPLE_ECHO and Path(EXAMPLE_ECHO).is_file(), '需要 CPR_EXAMPLE_ECHO_BIN 指向已构建的 example-echo')
    def test_real_example_echo_process_round_trip_through_platform(self):
        version = self.install('1')
        directory = Path(tempfile.mkdtemp(prefix='pe-'))
        self.addCleanup(shutil.rmtree, directory, True)
        socket_path = directory / 'echo.sock'
        process = subprocess.Popen([EXAMPLE_ECHO], env=dict(os.environ, CPR_PLUGIN_SOCKET=str(socket_path),
                                                              CPR_PLUGIN_DATA=str(directory / 'data'), CPR_PLUGIN_SHUTDOWN_MS='3000'))

        def stop():
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                process.wait(timeout=10)
        self.addCleanup(stop)
        for _ in range(500):
            if socket_path.exists():
                break
            time.sleep(0.01)
        self.write_registry(socket_path)
        status, body, _ = self.platform.call(ID, version, ID + '.echo', {'value': 'ok'}, 'session', 'admin', 'http://localhost')
        self.assertEqual((status, json.loads(body)), (200, {'value': 'ok'}))

    # ----- 生命周期经 updater -----

    def test_disable_and_enable_use_updater_and_keep_store_on_success(self):
        version = self.install('1')
        self.assertEqual(self.platform.lifecycle.begin(ID, 'disable', 'c', 'o')['status'], 'running')
        self.assertEqual(self.wait()['status'], 'succeeded')
        self.assertFalse(self.platform.store.state(ID)['enabled'])
        self.platform.lifecycle.begin(ID, 'enable', 'c', 'o')
        self.assertEqual(self.wait()['status'], 'succeeded')
        self.assertTrue(self.platform.store.state(ID)['enabled'])
        self.assertEqual(self.updater.posted(), [('disable', None), ('enable', version)])

    def test_failed_enable_restores_disabled_store_state(self):
        self.install('1')
        self.platform.store.action(ID, 'disable')
        self.updater.failing.add('enable')
        self.platform.lifecycle.begin(ID, 'enable', 'c', 'o')
        operation = self.wait()
        self.assertEqual(operation['status'], 'failed')
        self.assertIn('健康检查未通过', operation['message'])
        self.assertFalse(self.platform.store.state(ID)['enabled'])

    def test_failed_activation_on_upload_restores_previous_version_and_keeps_old_selection(self):
        first = self.install('1')
        before = self.platform.store.state(ID)
        second = self.platform.store.install(ID, *self.package('2'))['current']
        self.updater.failing.add('activate')
        self.platform.lifecycle.launch_service(ID, 'update', before)
        self.assertEqual(self.wait()['status'], 'failed')
        state = self.platform.store.state(ID)
        self.assertEqual(state['current'], first)
        self.assertIsNone(state['previous'])
        self.assertFalse((self.root / 'store' / ID / 'releases' / second).exists())
        self.assertEqual(self.updater.posted(), [('activate', second)])

    def test_failed_rollback_restores_current_selection(self):
        first = self.install('1')
        second = self.install('2')
        self.updater.failing.add('rollback')
        self.platform.lifecycle.begin(ID, 'rollback', 'c', 'o', second, first)
        self.assertEqual(self.wait()['status'], 'failed')
        state = self.platform.store.state(ID)
        self.assertEqual((state['current'], state['previous']), (second, first))

    def test_uninstall_restores_store_on_updater_failure_and_completes_on_success(self):
        version = self.install('1')
        self.updater.failing.add('uninstall')
        self.platform.lifecycle.begin(ID, 'uninstall', 'c', 'o')
        self.assertEqual(self.wait()['status'], 'failed')
        self.assertEqual(self.platform.store.state(ID)['current'], version)
        self.assertTrue(self.platform.store.state(ID)['enabled'])
        self.updater.failing.clear()
        self.platform.lifecycle.begin(ID, 'uninstall', 'c', 'o')
        self.assertEqual(self.wait()['status'], 'succeeded')
        self.assertIsNone(self.platform.store.state(ID)['current'])

    def test_missing_installer_is_refused_before_any_store_change(self):
        version = self.install('1')
        self.platform.lifecycle.updater_socket = None
        with self.assertRaisesRegex(ValueError, '独立安装器'):
            self.platform.lifecycle.begin(ID, 'disable', 'c', 'o')
        self.assertTrue(self.platform.store.state(ID)['enabled'])
        self.assertEqual(self.platform.store.state(ID)['current'], version)
        self.assertEqual(self.updater.requests, [])

    def test_store_rejects_non_elf_bad_paths_and_out_of_range_service_values(self):
        cases = [
            ('non-elf', {}, b'#!/bin/sh\n' + b'x' * 64, 'service/example-echo'),
            ('wrong-machine', {}, elf(b'x').replace((62).to_bytes(2, 'little'), (183).to_bytes(2, 'little')), 'service/example-echo'),
            ('traversal', {}, elf(b'x'), 'service/../example-echo'),
            ('outside-service-dir', {}, elf(b'x'), 'bin/example-echo'),
            ('memory-too-large', {'memoryMax': '1024M'}, elf(b'x'), 'service/example-echo'),
            ('shutdown-too-small', {'shutdownSeconds': 1}, elf(b'x'), 'service/example-echo'),
            ('unknown-capability', {'capabilities': ['shell.exec']}, elf(b'x'), 'service/example-echo'),
        ]
        for name, overrides, content, binary in cases:
            with self.subTest(name=name):
                directory = self.root / ('bad-' + name)
                (directory / 'service').mkdir(parents=True)
                (directory / 'service/example-echo').write_bytes(content)
                service = {**SERVICE, **overrides, 'binary': binary}
                manifest = {'id': ID, 'version': '9', 'protocol': 2, 'kind': 'ui', 'pages': [],
                            'capabilities': [ID + '.echo'],
                            'resources': {binary: hashlib.sha256(content).hexdigest()}, 'service': service}
                if binary != 'service/example-echo':
                    (directory / binary).parent.mkdir(parents=True, exist_ok=True)
                    (directory / binary).write_bytes(content)
                (directory / 'plugin.json').write_text(json.dumps(manifest))
                archive = self.root / ('bad-' + name + '.tar.gz')
                package_directory(directory, archive)
                with self.assertRaises(ValueError):
                    self.platform.store.install(ID, archive, hashlib.sha256(archive.read_bytes()).hexdigest())

    def test_service_operation_interrupted_by_control_plane_restart_is_marked_failed(self):
        journal = self.root / 'store' / 'lifecycle.json'
        journal.write_text(json.dumps({ID: {'status': 'running', 'kind': 'service', 'action': 'enable'}}))
        restarted = Lifecycle(self.platform)
        self.assertEqual(restarted.operations[ID]['status'], 'failed')
        self.assertIn('重启', restarted.operations[ID]['message'])


if __name__ == '__main__':
    unittest.main()

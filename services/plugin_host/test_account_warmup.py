"""Warmup management stays admin-only and follows the current enabled service."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from server import Platform, REPO
from service_calls import ServiceCalls, ServiceConflictError
import kernel_protocol


class WarmupBridgeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.platform = Platform(Path(self.temp.name) / 'store', 18336)
        self.caller = json.loads((REPO / 'plugins/accounts/plugin.json').read_text(encoding='utf-8'))
        self.target = json.loads((REPO / 'plugins/account-warmup/plugin.json').read_text(encoding='utf-8'))
        self.calls = Mock()
        self.calls.call.return_value = {'available': True, 'run': {'id': 'warmup_test', 'status': 'queued'}}
        self.platform.service_calls = self.calls

    def call(self, plugin='accounts', method='run', role='admin', data=None):
        return self.platform.call(plugin, 'old-page', 'account-warmup.' + method,
                                  data or {}, 'existing-session', role, 'http://localhost')

    def patches(self, enabled=True, target=None, caller=None):
        current = self.target if target is None else target
        source = self.caller if caller is None else caller
        return (patch.object(self.platform.store, 'state', side_effect=lambda plugin: {'enabled': enabled if plugin == 'account-warmup' else True, 'current': 'current-service'}),
                patch.object(self.platform.store, 'manifest', side_effect=lambda plugin, version=None: current if plugin == 'account-warmup' and version == 'current-service' else source))

    def test_accounts_only_forwards_declared_read_and_run_to_current_service(self):
        state, manifest = self.patches()
        with state, manifest:
            value = {'submissionId': 'clicked-once', 'scope': 'selected', 'accountIds': ['acct_one']}
            status, body, _ = self.call(data=value)
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body)['run']['status'], 'queued')
            self.calls.call.assert_called_once_with('account-warmup', 'run', value, 'admin')
            self.calls.reset_mock()
            self.call(method='read')
            self.calls.call.assert_called_once_with('account-warmup', 'read', {}, 'admin')

    def test_no_cookie_is_forwarded_and_key_role_cannot_warm_accounts(self):
        state, manifest = self.patches()
        with state, manifest:
            for role in ['key', '', 'viewer']:
                with self.assertRaises(PermissionError):
                    self.call(role=role)
            self.calls.call.assert_not_called()

    def test_foreign_plugin_and_accounts_cannot_gain_configuration_capability(self):
        granted = {**self.caller, 'capabilities': self.target['capabilities']}
        state, manifest = self.patches(caller=granted)
        with state, manifest:
            for plugin, method in [('groups', 'read'), ('groups', 'run'), ('accounts', 'configure'), ('accounts', 'history'), ('accounts', 'cancel')]:
                with self.subTest(plugin=plugin, method=method), self.assertRaises(PermissionError):
                    self.call(plugin=plugin, method=method)
            self.calls.call.assert_not_called()

    def test_old_page_does_not_bypass_removed_current_service_permission(self):
        current = {**self.target, 'capabilities': ['account-warmup.read']}
        state, manifest = self.patches(target=current)
        with state, manifest, self.assertRaises(PermissionError):
            self.call()
        self.calls.call.assert_not_called()
        state, manifest = self.patches(target={key: value for key, value in self.target.items() if key != 'service'})
        with state, manifest, self.assertRaises(PermissionError):
            self.call()

    def test_disabled_read_is_explicitly_unavailable_and_run_sends_nothing(self):
        state, manifest = self.patches(enabled=False)
        with state, manifest:
            _, body, _ = self.call(method='read')
            self.assertEqual(json.loads(body), {'available': False, 'reason': 'plugin_disabled'})
            with self.assertRaises(ConnectionError):
                self.call()
        self.calls.call.assert_not_called()

    def test_missing_service_registry_is_not_false_acceptance(self):
        self.platform.service_calls = None
        state, manifest = self.patches()
        with state, manifest:
            _, body, _ = self.call(method='read')
            self.assertEqual(json.loads(body), {'available': False, 'reason': 'service_unavailable'})
            with self.assertRaises(ConnectionError):
                self.call()

    def test_warmup_page_can_configure_but_still_follows_current_manifest(self):
        state, manifest = self.patches(caller=self.target)
        with state, manifest:
            value = {'expectedRevision': 1, 'settings': {'enabled': False}}
            self.call(plugin='account-warmup', method='configure', data=value)
            self.calls.call.assert_called_once_with('account-warmup', 'configure', value, 'admin')

    def test_unreachable_socket_disables_read_but_never_accepts_a_run(self):
        self.calls.call.side_effect = ConnectionError('unreachable socket')
        state, manifest = self.patches()
        with state, manifest:
            _, body, _ = self.call(method='read')
            self.assertEqual(json.loads(body), {'available': False, 'reason': 'service_unavailable'})
            with self.assertRaises(ConnectionError):
                self.call()

    def test_backend_permission_failure_is_not_hidden_as_unavailable(self):
        self.calls.call.side_effect = PermissionError('forbidden')
        state, manifest = self.patches()
        with state, manifest, self.assertRaises(PermissionError):
            self.call(method='read')

    def test_missing_caller_declaration_and_non_object_parameters_are_rejected(self):
        state, manifest = self.patches(caller={**self.caller, 'capabilities': []})
        with state, manifest, self.assertRaises(PermissionError):
            self.call()
        state, manifest = self.patches()
        with state, manifest, self.assertRaises(ValueError):
            self.platform.call('accounts', 'old-page', 'account-warmup.run', [], 'cookie', 'admin', 'http://localhost')


class WarmupConflictTests(unittest.TestCase):
    def test_admin_call_binds_socket_handshake_to_registered_plugin(self):
        calls = ServiceCalls('/unused/registry.json')
        with patch.object(calls, 'socket', return_value='/unused/plugin.sock'), patch(
            'kernel_protocol.admin_call', return_value={'available': True}
        ) as admin:
            self.assertEqual(calls.call('account-warmup', 'read', {}, 'admin'), {'available': True})
        admin.assert_called_once_with(
            '/unused/plugin.sock', 'read', {}, 'admin', 10, expected_id='account-warmup'
        )

    def test_revision_conflict_is_distinct_from_unavailable(self):
        calls = ServiceCalls('/unused/registry.json')
        with patch.object(calls, 'socket', return_value='/unused/plugin.sock'), patch('kernel_protocol.admin_call', side_effect=kernel_protocol.RpcFailure('conflict', 'revision')):
            with self.assertRaises(ServiceConflictError):
                calls.call('account-warmup', 'configure', {}, 'admin')

    def test_transport_error_is_a_connection_error_for_read_availability(self):
        calls = ServiceCalls('/unused/registry.json')
        with patch.object(calls, 'socket', return_value='/unused/plugin.sock'), patch('kernel_protocol.admin_call', side_effect=kernel_protocol.ProtocolError('truncated response')):
            with self.assertRaises(ConnectionError):
                calls.call('account-warmup', 'read', {}, 'admin')


if __name__ == '__main__':
    unittest.main()

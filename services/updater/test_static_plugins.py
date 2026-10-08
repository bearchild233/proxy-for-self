import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from static_plugins import StaticPluginStore


class StaticPluginTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.store = StaticPluginStore(self.root / 'store', {
            'groups': {'name': 'Groups', 'required': True, 'capabilities': ['groups.list']},
            'extra': {'name': 'Extra', 'required': False, 'capabilities': ['groups.list']},
        })

    def bundle(self, version='1', plugin='groups', modify=None, extra=None, component=False):
        html = ('<h1>version ' + version + '</h1>').encode()
        manifest = {'id': plugin, 'version': version, 'protocol': 2, 'kind': 'ui',
                    'resources': {'ui/index.html': hashlib.sha256(html).hexdigest()},
                    'capabilities': ['groups.list'],
                    'pages': [{'id': 'main', 'title': 'Groups', 'entry': 'ui/index.html', 'roles': ['admin']}]}
        entry = 'ui/module.js' if component else 'ui/index.html'
        if component:
            manifest.update(runtime='vue-component', hostApi=1)
            manifest['pages'][0]['entry'] = entry
            manifest['resources'] = {entry: hashlib.sha256(html).hexdigest()}
        if modify:
            modify(manifest)
        archive = self.root / 'bundle.tar.gz'
        with tarfile.open(archive, 'w:gz') as output:
            for name, data in [('plugin.json', json.dumps(manifest).encode()), (entry, html), *(extra or [])]:
                item = tarfile.TarInfo(name)
                item.size = len(data)
                output.addfile(item, io.BytesIO(data))
        return archive, hashlib.sha256(archive.read_bytes()).hexdigest()

    def install(self, version='1', plugin='groups', **kwargs):
        return self.store.install(plugin, *self.bundle(version, plugin, **kwargs))

    def test_component_runtime_requires_host_trust_and_compatible_api(self):
        original = self.install()['current']
        with self.assertRaisesRegex(ValueError, '未授权'):
            self.install('2', component=True)
        self.assertEqual(self.store.state('groups')['current'], original)
        self.store.policies['groups']['trustedUi'] = True
        for value in [0, 2, None]:
            with self.assertRaises(ValueError):
                self.install('2', component=True, modify=lambda m: m.update(hostApi=value))
        active = self.install('2', component=True)['current']
        self.assertIn('version 2', self.store.page('groups', active, 'main', 'admin'))
        with self.assertRaises(ValueError):
            self.store.page('groups', active, 'main', 'key')
        self.assertEqual(self.store.action('groups', 'rollback')['current'], original)

    def test_update_and_rollback_keep_only_two_releases(self):
        first = self.install()['current']
        second = self.install('2')['current']
        self.assertIn('version 1', self.store.page('groups', first, 'main', 'admin'))
        self.assertEqual(self.store.action('groups', 'rollback')['current'], first)
        self.assertEqual(self.store.action('groups', 'rollback')['current'], second)
        third = self.install('3')['current']
        releases = self.store.directory('groups') / 'releases'
        self.assertEqual({item.name for item in releases.iterdir()}, {second, third})
        with self.assertRaises(ValueError):
            self.store.page('groups', first, 'main', 'admin')

    def test_rejected_updates_preserve_active_page(self):
        state = self.install()
        invalid = [
            {'modify': lambda m: m.update(capabilities=['secrets.read'])},
            {'modify': lambda m: m['resources'].update({'ui/index.html': 'bad'})},
            {'extra': [('../outside', b'x')]},
            {'extra': [('secret.txt', b'x')]},
            {'modify': lambda m: m['pages'][0].update(roles=[{}])},
            {'modify': lambda m: m['pages'][0].update(entry={})},
        ]
        for case in invalid:
            with self.subTest(case=case), self.assertRaises(ValueError):
                self.install('2', **case)
            self.assertEqual(state, self.store.state('groups'))
            self.assertIn('version 1', self.store.page('groups', state['current'], 'main', 'admin'))

    def test_release_notes_and_stale_rollback_target(self):
        first = self.install()['current']
        second = self.install('2', modify=lambda m: m.update(
            releaseNotes=['修复表格高度'], releasedAt='2026-10-08T15:31:54Z'))['current']
        self.assertEqual(self.store.manifest('groups')['releaseNotes'], ['修复表格高度'])
        third = self.install('3')['current']
        with self.assertRaisesRegex(ValueError, '版本已变化'):
            self.store.action('groups', 'rollback', second, first)
        self.assertEqual(self.store.state('groups')['current'], third)
        self.assertEqual(self.store.action('groups', 'rollback', third, second)['current'], second)
        for metadata in [{'releaseNotes': 'not a list'}, {'releasedAt': '2026-10-08'}]:
            with self.subTest(metadata=metadata), self.assertRaises(ValueError):
                self.install('bad', modify=lambda m: m.update(metadata))
        self.assertEqual(self.store.state('groups')['current'], second)

    def test_archive_hash_is_checked(self):
        archive, _ = self.bundle()
        with self.assertRaises(ValueError):
            self.store.install('groups', archive, '0' * 64)
        self.assertIsNone(self.store.state('groups')['current'])

    def test_failed_atomic_activation_keeps_previous_state(self):
        state = self.install()
        with patch.object(self.store, 'save', side_effect=OSError('disk full')):
            with self.assertRaises(OSError):
                self.install('2')
        self.assertEqual(self.store.state('groups'), state)
        self.assertIn('version 1', self.store.page('groups', state['current'], 'main', 'admin'))

    def test_required_cannot_disable_uninstall(self):
        state = self.install()
        for action in ('disable', 'uninstall'):
            with self.assertRaises(ValueError):
                self.store.action('groups', action)
            self.assertEqual(state, self.store.state('groups'))

    def test_optional_disabled_stays_disabled_after_update_and_uninstall_cleans(self):
        self.install(plugin='extra')
        self.store.action('extra', 'disable')
        state = self.install('2', plugin='extra')
        self.assertFalse(state['enabled'])
        self.assertEqual(self.store.catalog('admin'), [])
        with self.assertRaises(ValueError):
            self.store.page('extra', state['current'], 'main', 'admin')
        self.store.action('extra', 'enable')
        self.assertEqual(len(self.store.catalog('admin')), 1)
        self.store.action('extra', 'uninstall')
        self.assertEqual(list((self.store.directory('extra') / 'releases').iterdir()), [])

    def test_role_and_missing_previous(self):
        state = self.install()
        self.assertEqual(self.store.catalog('key'), [])
        with self.assertRaises(ValueError):
            self.store.page('groups', state['current'], 'main', 'key')
        with self.assertRaises(ValueError):
            self.store.action('groups', 'rollback')
        self.assertEqual(state, self.store.state('groups'))


if __name__ == '__main__':
    unittest.main()

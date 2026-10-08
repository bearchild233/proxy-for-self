import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch, MagicMock

from server import Platform, REPO, package_directory, CLIENT_IP

spec=importlib.util.spec_from_file_location('diagnostic_worker',REPO/'plugins/account-diagnostics/worker/main.py')
worker=importlib.util.module_from_spec(spec);spec.loader.exec_module(worker)


class DiagnosticRules(unittest.TestCase):
    def test_bootstrap_then_one_probe_classifies_only_sent_material(self):
        for returned,expected in [(True,'29'),(False,'21')]:
            calls=[]
            def observe(data):
                calls.append(data)
                if data['input']['phase']=='prepare':return {'prepared':True,'materialRef':'old','bootstrap':True}
                return {'status':200,'stateReturned':returned,'sentMaterialRef':'old','materialRef':'new-secret-reference'}
            result=worker.run({'accountId':'acct_test','mode':'quick'},observe)
            self.assertEqual(result['classification'],expected)
            self.assertEqual(len(calls),2)
            self.assertEqual(calls[1]['input']['materialRef'],'old')
            self.assertNotIn('materialRef',result)
            self.assertNotIn('sentMaterialRef',result)

    def test_failures_and_unknown_material_are_not_classified(self):
        for status,sent in [(401,'old'),(429,'old'),(None,'old'),(200,'wrong')]:
            values=iter([{'prepared':True,'materialRef':'old'},{'status':status,'sentMaterialRef':sent,'stateReturned':True}])
            self.assertIsNone(worker.run({'accountId':'acct_a','mode':'quick'},lambda _:next(values))['classification'])
        calls=[]
        result=worker.run({'accountId':'acct_a','mode':'quick'},lambda data:calls.append(data) or {'prepared':False,'status':200})
        self.assertEqual(len(calls),1);self.assertIsNone(result['classification'])


class PlatformContracts(unittest.TestCase):
    def test_forward_supplies_trusted_proxy_address_and_resets_context(self):
        connection = MagicMock()
        connection.getresponse.return_value.read.return_value = b'{}'
        token = CLIENT_IP.set('198.51.100.42')
        try:
            with patch('server.HTTPConnection', return_value=connection):
                self.platform.forward('GET', '/api/auth/status')
            self.assertEqual(connection.request.call_args.args[3]['X-Real-IP'], '198.51.100.42')
        finally:
            CLIENT_IP.reset(token)
        self.assertEqual(CLIENT_IP.get(), '127.0.0.1')

    def test_reference_key_binding_lookup_is_allowed_but_mutations_are_not(self):
        operation = 'api-keys.post.admin.client-keys.bindings'
        for plugin in ('groups', 'usage-analytics', 'client-access'):
            manifest = json.loads((REPO / 'plugins' / plugin / 'plugin.json').read_text(encoding='utf-8'))
            self.assertIn(operation, self.platform.policies[plugin]['capabilities'])
            with patch.object(self.platform.store, 'state', return_value={'enabled': True}), patch.object(self.platform.store, 'manifest', return_value=manifest), patch.object(self.platform, 'forward', return_value=(200, b'{}', None)) as forward:
                self.platform.call(plugin, 'version', operation, {'ids': ['existing-key']}, 'cookie', 'admin', 'https://example.test')
                self.assertEqual(forward.call_args.args[:2], ('POST', '/api/admin/client-keys/bindings'))
                with self.assertRaises(PermissionError):
                    self.platform.call(plugin, 'version', operation, {'ids': ['existing-key']}, 'cookie', 'key', 'https://example.test')
                with self.assertRaises(PermissionError):
                    self.platform.call(plugin, 'version', 'api-keys.post.admin.client-keys.set-binding', {}, 'cookie', 'admin', 'https://example.test')

    def test_lazy_loading_is_opt_in_persistent_and_strict_boolean(self):
        self.assertEqual(self.platform.loading_preferences(), {})
        self.platform.set_loading('groups', True)
        reopened = Platform(self.platform.root, 18336)
        self.assertIs(reopened.loading_preferences()['groups'], True)
        self.platform.set_loading('groups', False)
        self.assertIs(self.platform.loading_preferences()['groups'], False)
        with self.assertRaises(ValueError):
            self.platform.set_loading('groups', 'false')
        with self.assertRaises(ValueError):
            self.platform.set_loading('unknown', True)

    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.platform=Platform(Path(self.temp.name)/'store',18336)

    def package(self,id='sample',version='1',worker_code=None):
        directory=Path(self.temp.name)/('package-'+version);directory.mkdir()
        files={'ui.html':'<h1>'+version+'</h1>'}
        if worker_code:files['worker.py']=worker_code
        for name,value in files.items():(directory/name).write_text(value,encoding='utf-8',newline='\n')
        capabilities=['tasks.start','tasks.status'] if worker_code else []
        manifest={'id':id,'version':version,'protocol':2,'kind':'ui','pages':[{'id':'main','title':'Test','entry':'ui.html','roles':['admin'],'route':'/extensions/'+id+'/main'}],'capabilities':capabilities,'resources':{name:hashlib.sha256(value.encode()).hexdigest() for name,value in files.items()}}
        if worker_code:manifest['worker']='worker.py'
        (directory/'plugin.json').write_text(json.dumps(manifest))
        archive=Path(self.temp.name)/('package-'+version+'.tar.gz');package_directory(directory,archive)
        return archive,hashlib.sha256(archive.read_bytes()).hexdigest()

    def register(self,worker=False):
        self.platform.register({'id':'sample','name':'Sample','description':'Test','capabilities':['tasks.start','tasks.status'] if worker else [],'worker':worker})

    def test_registration_required_protection_and_capabilities(self):
        self.register()
        self.platform.store.install('sample',*self.package())
        self.assertTrue(self.platform.catalog('admin')[0]['enabled'])
        with self.assertRaises(ValueError):self.platform.store.action('accounts','disable')
        with self.assertRaises(PermissionError):self.platform.call('sample',self.platform.store.state('sample')['current'],'accounts.get.admin.accounts',{},'session','admin','http://127.0.0.1:18335')
        self.platform.store.action('sample','disable')
        self.assertEqual(self.platform.catalog('admin'),[])
        reloaded=Platform(self.platform.root,18336)
        self.assertIn('sample',reloaded.policies)

    def test_management_catalog_identifies_both_releases(self):
        self.register()
        first = self.platform.store.install('sample', *self.package())['current']
        second = self.platform.store.install('sample', *self.package(version='2'))['current']
        item = next(p for p in self.platform.catalog('admin', True) if p['id'] == 'sample')
        self.assertEqual(item['currentRelease']['contentVersion'], second)
        self.assertEqual(item['previousRelease']['contentVersion'], first)
        self.assertEqual(item['previousRelease']['version'], '1')
        self.assertEqual(item['previousRelease']['notes'], [])
        self.assertIsNone(item['previousRelease']['releasedAt'])
        self.assertNotIn('previousRelease', self.platform.catalog('admin')[0])

    def test_lease_preserves_old_form_across_multiple_updates(self):
        self.register();store=self.platform.store
        first=store.install('sample',*self.package())['current'];lease=store.lease('sample',first)
        store.install('sample',*self.package(version='2'));store.install('sample',*self.package(version='3'))
        self.assertEqual(store.page('sample',first,'main','admin'),'<h1>1</h1>')
        store.lease('sample',first,lease,release=True)
        with self.assertRaises(ValueError):store.manifest('sample',first)

    def test_task_drains_pinned_version_and_is_owned_by_session(self):
        self.register(True)
        code="import time,json,sys\njson.loads(sys.stdin.readline())\ntime.sleep(.2)\nprint(json.dumps({'result':{'version':'old'}}),flush=True)\n"
        version=self.platform.store.install('sample',*self.package(worker_code=code))['current']
        task=self.platform.jobs.start('sample',version,{},'session','admin','http://127.0.0.1:18335')
        self.platform.store.install('sample',*self.package(version='2',worker_code=code.replace("'old'","'new'")))
        self.platform.store.action('sample','disable')
        with self.assertRaises(PermissionError):self.platform.jobs.start('sample',version,{},'session','admin','http://127.0.0.1:18335')
        with self.assertRaises(PermissionError):self.platform.jobs.get(task['id'],'other')
        for _ in range(60):
            result=self.platform.jobs.get(task['id'],'session')
            if result['status']!='running':break
            time.sleep(.05)
        self.assertEqual(result['result']['version'],'old')


class BackupPolicyTests(unittest.TestCase):
    def test_independent_policy_and_disable_gate(self):
        from policy_runner import decide
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);version='a'*64
            package=root/'plugins/backup/releases'/version
            (package/'worker').mkdir(parents=True)
            content=(REPO/'plugins/backup/worker/policy.py').read_bytes()
            (package/'worker/policy.py').write_bytes(content)
            (package/'plugin.json').write_text(json.dumps({'sdk':1,'policyWorker':'worker/policy.py','resources':{'worker/policy.py':hashlib.sha256(content).hexdigest()}}))
            state=root/'plugins/backup/active.json'
            state.write_text(json.dumps({'enabled':True,'current':version}))
            self.assertTrue(decide(root,{'action':'schedule','due':True})['schedule'])
            result=decide(root,{'action':'retention','now':1000000,'retentionCount':1,'retentionDays':0,'records':[{'id':'newest','completedAt':900000},{'id':'old','completedAt':800000}]})
            self.assertEqual(result['delete'],['old'])
            state.write_text(json.dumps({'enabled':False,'current':version}))
            self.assertEqual(decide(root,{'action':'admission'}),{'enabled':False})
            state.write_text(json.dumps({'enabled':True,'current':version}))
            (package/'worker/policy.py').write_text('corruption')
            with self.assertRaises(ValueError):decide(root,{'action':'admission'})


if __name__=='__main__':unittest.main()

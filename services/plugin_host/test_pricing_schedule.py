import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from datetime import datetime
from server import Platform, REPO, package_directory
from policy_runner import decide as run_policy

spec = importlib.util.spec_from_file_location('pricing_policy', REPO/'plugins/pricing/worker/policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)

class PricingScheduleTests(unittest.TestCase):
    def test_daily_boundary_uses_fixed_china_time(self):
        for value in ['2026-03-08T18:59:00+00:00', '2026-11-01T18:59:00+00:00']:
            now = int(datetime.fromisoformat(value).timestamp())
            self.assertEqual(policy.next_daily(now), now+60)
            self.assertEqual(policy.next_daily(now+60), now+60+86400)

    def test_failure_retries_and_old_completion_cannot_overwrite(self):
        state = {'nextRun': 100}
        claim = policy.decide({'action':'claim','now':100,'runId':'first','state':state})
        self.assertTrue(claim['run'])
        again = policy.decide({'action':'claim','now':101,'runId':'second','state':claim['state']})
        self.assertFalse(again['run'])
        failure = policy.decide({'action':'finish','now':120,'runId':'first','success':False,'state':claim['state']})
        self.assertEqual(failure['state']['nextRun'], 3720)
        stale = policy.decide({'action':'finish','now':121,'runId':'other','success':True,'state':failure['state']})
        self.assertEqual(stale['state'], failure['state'])

    def test_persisted_schedule_survives_restart_and_disable_blocks_claim(self):
        with tempfile.TemporaryDirectory() as temporary:
            platform = Platform(Path(temporary)/'store',18336)
            source = Path(temporary)/'package'
            (source/'worker').mkdir(parents=True)
            content = (REPO/'plugins/pricing/worker/policy.py').read_bytes()
            (source/'worker/policy.py').write_bytes(content)
            (source/'ui.html').write_text('<h1>pricing</h1>')
            manifest = {'id':'pricing','version':'test','protocol':2,'sdk':1,'kind':'ui','policyWorker':'worker/policy.py',
                        'capabilities':[],'pages':[{'id':'main','title':'Pricing','entry':'ui.html','roles':['admin']}],
                        'resources':{'worker/policy.py':hashlib.sha256(content).hexdigest(), 'ui.html':hashlib.sha256((source/'ui.html').read_bytes()).hexdigest()}}
            (source/'plugin.json').write_text(json.dumps(manifest))
            archive=Path(temporary)/'pricing.tar.gz';package_directory(source,archive)
            platform.store.install('pricing',archive,hashlib.sha256(archive.read_bytes()).hexdigest())
            run_policy(platform.root, {'action':'claim','now':100}, 'pricing')
            state_path=platform.root/'pricing-runtime'/'status.json'
            next_run=json.loads(state_path.read_text())['nextRun']
            self.assertFalse(run_policy(platform.root, {'action':'claim','now':101}, 'pricing')['run'])
            claim=run_policy(platform.root, {'action':'claim','now':next_run}, 'pricing')
            self.assertTrue(claim['run'])
            run_policy(platform.root, {'action':'finish','now':next_run+5,'runId':claim['runId'],'success':True}, 'pricing')
            state=json.loads(state_path.read_text())
            self.assertEqual(state['lastSuccess'],next_run+5)
            self.assertGreater(state['nextRun'],next_run+5)
            platform.store.action('pricing','disable')
            self.assertFalse(run_policy(platform.root, {'action':'claim','now':state['nextRun']}, 'pricing')['enabled'])

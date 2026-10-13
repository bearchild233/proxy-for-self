import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor

from gateway_pool import GatewayPool

REPO = Path(__file__).resolve().parents[2]


class Store:
    enabled = True

    def __init__(self, root):
        self.path = Path(root) / 'plugins/gateway-pool'
        release = self.path / 'releases' / ('a'*64)
        (release/'worker').mkdir(parents=True)
        policy = (REPO/'plugins/gateway-pool/worker/policy.py').read_bytes()
        (release/'worker/policy.py').write_bytes(policy)
        self.document = {'policyWorker':'worker/policy.py', 'resources':{'worker/policy.py':hashlib.sha256(policy).hexdigest()}}

    def directory(self, _): return self.path
    def state(self, _): return {'enabled':self.enabled, 'current':'a'*64}
    def manifest(self, *_): return self.document


class GatewayPoolTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.now = 10000
        self.store = Store(self.temp.name)
        self.pool = GatewayPool(self.temp.name, self.store, lambda:self.now)
        self.pool.configure({'gateways':['unified-142','unified-143'], 'initialRestSeconds':3600})

    def call(self, action, **data):
        return self.pool.runtime({'action':action, 'accountId':'A', **data})

    def probe(self, state=False, **data):
        claim = self.call('claim', demand=True)
        self.assertTrue(claim['claimed'])
        lease = claim['leaseId']
        self.call('sent', leaseId=lease)
        result = self.call('probeResult', leaseId=lease, status=200, stateReturned=state, paired=True, gatewayMatches=True, **data)
        self.call('finish', leaseId=lease)
        return claim['pair'], result

    def test_model_ticket_shares_window_and_expires_with_generation(self):
        self.assertEqual(self.pool.settings_for_ui()['probeModel'], 'gpt-6.1-sol')
        pair, _ = self.probe(False)
        generation = pair['generation']
        reserved = self.call('reserve', gateway=pair['gateway'], generation=generation, deadline=self.now+60)
        lease = reserved['leaseId']
        ticket = {'state':'astra-state','cookie':'__oailb=x; __cflb=y','expires':self.now+600}
        # 未实际发送的租约不能记录拿票。
        with self.assertRaises(ValueError):
            self.call('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, stateReturned=True, material=ticket)
        self.call('sent', leaseId=lease)
        self.call('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, stateReturned=True, durationMs=5, material=ticket)
        self.assertEqual(self.call('ticket', generation=generation, model='gpt-6-astra'), ticket)
        self.assertEqual(self.pair()['ticketModels'], ['gpt-6-astra'])
        self.assertNotIn('astra-state', json.dumps(self.pool.view()))
        self.assertIsNone(self.call('ticket', generation=generation, model='gpt-6.1-sol'))
        sample = self.pool.view()['observations'][0]
        self.assertEqual((sample['source'], sample['model'], sample['classification']), ('ticket', 'gpt-6-astra', None))
        # 拿票不做判定，窗口仍是同一组合；29 退役后该代次的票全部作废。
        self.assertEqual(self.pair()['state'], 'ready')
        self.call('businessResult', leaseId=lease, model='gpt-6-astra', status=200, stateReturned=True, paired=True, gatewayMatches=True)
        self.call('finish', leaseId=lease)
        self.assertEqual(self.pair()['state'], 'cooling')
        self.assertIsNone(self.call('ticket', generation=generation, model='gpt-6-astra'))
        with self.assertRaises(ValueError):
            self.call('ticket', generation=generation, model='GPT 6')

    def test_candidates_follow_pool_catalog_and_cookie_expiry_rotates(self):
        self.pool.configure({'gateways': []})
        catalog = [{'gateway': 'unified-150', 'validForS': 100, 'country': 'CH'},
                   {'gateway': 'unified-151', 'validForS': 2000, 'country': 'US'}]
        # 不在目录中的网关不是候选；Cookie 剩余不足一个窗口的排在后面。
        claim = self.call('claim', demand=True, catalog=catalog)
        self.assertEqual(claim['pair']['gateway'], 'unified-151')
        lease = claim['leaseId']
        self.call('sent', leaseId=lease)
        material = {'state': 's0', 'cookie': '__oailb=x; __cflb=y', 'expires': self.now + 600}
        self.call('probeResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True, material=material)
        self.call('finish', leaseId=lease)
        pair = self.pair(gateway='unified-151')
        self.assertEqual((pair['state'], pair['cookieExpiresAt']), ('ready', self.now + 600))
        self.assertEqual(self.pool.view()['catalogs']['A']['gateways'][0]['country'], 'CH')
        # 窗口未到，但 Cookie 即将到期：开始准备下一个；目录里已无此网关也继续服务。
        self.now += 100
        plan = self.call('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 3000}])
        self.assertFalse(plan['rotationDue'])
        self.assertEqual(plan['ready']['gateway'], 'unified-151')
        self.now += 450
        plan = self.call('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 3000}])
        self.assertTrue(plan['rotationDue'])
        self.assertEqual(plan['probe']['gateway'], 'unified-150')
        with self.assertRaises(ValueError):
            self.call('plan', demand=True, catalog=[{'gateway': 'evil', 'validForS': 1}])

    def test_windows_are_independent_per_account(self):
        pair, _ = self.probe()
        self.assertEqual(self.pool.runtime({'action': 'active'})['accounts'], ['A'])
        run = lambda action, **data: self.pool.runtime({'action': action, 'accountId': 'B', **data})
        # A 的窗口未到期：B 属于另一个 Key 的选择，可以独立开自己的窗口。
        claim = run('claim', demand=True)
        self.assertTrue(claim['claimed'])
        run('sent', leaseId=claim['leaseId'])
        run('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=True)
        run('finish', leaseId=claim['leaseId'])
        self.assertEqual(sorted(self.pool.runtime({'action': 'active'})['accounts']), ['A', 'B'])
        self.assertEqual(self.pair()['state'], 'ready')
        self.assertEqual(self.pair('B')['state'], 'ready')
        switch = self.pool.view(account='B')['switches'][0]
        self.assertEqual((switch['fromAccount'], switch['fromGateway']), (None, None))

    def pair(self, account='A', gateway='unified-142'):
        return next(p for p in self.pool.view()['pairs'] if p['accountId']==account and p['gateway']==gateway)

    def test_member_identity_mapping_accepts_complete_upstream_member(self):
        identity = 'gwpool-member:account-uuid/user.member_1'
        self.pool.configure({'poolAccountIds': {'A': identity}})
        self.assertEqual(self.pool.settings_for_ui()['poolAccountIds']['A'], identity)
        with self.assertRaises(ValueError):
            self.pool.configure({'poolAccountIds': {'A': 'member\ninvalid'}})

    def test_probe_prepares_once_and_switch_preserves_inflight(self):
        pair, result = self.probe()
        self.assertEqual(result['classification'], '21')
        lease = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+500)['leaseId']
        self.call('sent', leaseId=lease)
        self.now += 181
        self.probe()
        old = self.pair()
        self.assertEqual(old['state'], 'draining')
        self.assertFalse(self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+5)['reserved'])
        self.now += 10
        self.call('finish', leaseId=lease)
        self.assertEqual(self.pair()['restUntil'], self.now+3600)

    def test_business_21_is_not_retired_and_29_retires(self):
        pair, _ = self.probe()
        for returned, verdict in [(False,'21'), (True,'29')]:
            lease = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+100)['leaseId']
            self.call('sent', leaseId=lease)
            self.assertEqual(self.call('businessResult', leaseId=lease, status=200, stateReturned=returned, paired=True, gatewayMatches=True)['classification'], verdict)
            self.call('finish', leaseId=lease)
        self.assertEqual(self.pair()['state'], 'cooling')

    def test_29_then_replacement_keeps_previous_gateway_and_window(self):
        pair, _ = self.probe()
        lease = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+100)['leaseId']
        self.call('sent', leaseId=lease)
        self.call('businessResult', leaseId=lease, status=200, stateReturned=True, paired=True, gatewayMatches=True)
        self.call('finish', leaseId=lease)
        self.now += 10
        self.probe()
        switch = self.pool.view()['switches'][0]
        self.assertEqual(switch['fromGateway'], 'unified-142')
        self.assertEqual(switch['toGateway'], 'unified-143')
        self.assertEqual(switch['previousWindowSeconds'], 10)

    def test_cooldown_restarts_at_each_probe_and_records_precontact_idle(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe(True)
        self.now += 3600
        self.probe(True)
        self.assertEqual(self.pair()['restUntil'], self.now+7200)
        self.assertEqual(self.pool.view()['observations'][0]['observedIdleSeconds'], 3600)
        self.now += 7200
        self.probe(False)
        pair = self.pair()
        self.assertEqual(pair['lastRecoveredRestSeconds'], 7200)
        self.assertIsNone(pair.get('learnedRestSeconds'))

    def test_two_equal_recoveries_learn_and_different_recovery_resets(self):
        policy = self.pool.policy()
        pair, settings = {}, {**policy.DEFAULTS, 'initialRestSeconds': 3600}
        policy.recover(pair, settings, 7200)
        self.assertEqual(policy.initial(settings, pair.get('learnedRestSeconds')), 3600)
        policy.recover(pair, settings, 7200)
        self.assertEqual(policy.initial(settings, pair['learnedRestSeconds']), 7200)
        policy.recover(pair, settings, 14400)
        self.assertEqual(policy.initial(settings, pair.get('learnedRestSeconds')), 3600)
        policy.recover(pair, settings, 14400)
        self.assertEqual(policy.initial(settings, pair['learnedRestSeconds']), 14400)

    def test_lazy_has_no_idle_preparation_continuous_does(self):
        self.probe()
        self.now += 181
        self.assertIsNone(self.call('plan', demand=False)['probe'])
        self.pool.configure({'mode':'continuous'})
        self.assertIsNotNone(self.call('plan', demand=False)['probe'])

    def test_restart_keeps_cooldown_configuration_and_samples(self):
        self.pool.configure({'initialRestSeconds':1800, 'poolToken':'fake-pool-secret'})
        self.probe(True)
        self.pool = GatewayPool(self.temp.name, self.store, lambda:self.now)
        self.assertEqual(self.pair()['restUntil'], self.now+1800)
        visible = self.pool.view()
        self.assertNotIn('fake-pool-secret', json.dumps(visible))
        self.assertTrue(visible['settings']['poolTokenConfigured'])
        self.assertEqual(len(visible['observations']), 1)

    def test_simultaneous_ab_claims_only_prepare_once(self):
        other = GatewayPool(self.temp.name, self.store, lambda:self.now)
        with ThreadPoolExecutor(2) as executor:
            results = list(executor.map(lambda p:p.runtime({'action':'claim','accountId':'A','demand':True}), [self.pool,other]))
        self.assertEqual(sum(r['claimed'] for r in results), 1)

    def test_accounts_cool_independently_on_same_gateway(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe(True)
        self.assertFalse(self.call('claim', demand=True)['claimed'])
        self.assertTrue(self.pool.runtime({'action':'claim','accountId':'B','demand':True})['claimed'])

    def test_unknown_errors_and_gateway_change_never_classify_21(self):
        for status, matched in [(401,True), (429,True), (200,False)]:
            claim = self.pool.runtime({'action':'claim','accountId':str(status)+str(matched), 'demand':True})
            result = self.pool.runtime({'action':'probeResult','accountId':str(status)+str(matched),'leaseId':claim['leaseId'], 'status':status,'paired':True,'gatewayMatches':matched,'stateReturned':False})
            self.assertIsNone(result['classification'])

    def test_maximum_backoff_and_configuration_is_not_reset(self):
        policy = self.pool.policy()
        pair = {'state':'preparing','probeRestSeconds':57600}
        policy.retire(pair, policy.DEFAULTS, degraded=True)
        self.assertEqual(pair['restSeconds'], 86400)
        for settings in [{'initialRestSeconds':90000}, {'poolUrl':'http://example.com'}, {'maximumRestSeconds':1}, {'gateways':['bad']}, {'poolToken':'a\nb'}]:
            with self.assertRaises(ValueError): self.pool.configure(settings)

    def test_no_probe_contact_does_not_restart_idle_and_crash_is_recoverable(self):
        claim = self.call('claim', demand=True)
        self.now += 61
        self.call('plan', demand=True)
        self.assertEqual(self.pair()['state'], 'unknown')
        self.assertNotIn('lastContactAt', self.pair())
        self.assertFalse(self.call('finish', leaseId=claim['leaseId'])['accepted'])

    def test_window_and_observed_idle_start_at_actual_send_not_claim(self):
        claim = self.call('claim', demand=True)
        self.now += 5
        self.call('sent', leaseId=claim['leaseId'])
        first = self.now
        self.now += 3
        self.call('sent', leaseId=claim['leaseId'])
        self.call('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=True)
        self.call('finish', leaseId=claim['leaseId'])
        self.assertEqual(self.pair()['firstContactAt'], first)

    def test_switch_rejects_unsent_reservation_but_keeps_sent_request(self):
        pair, _ = self.probe()
        pending = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+500)['leaseId']
        self.now += 181
        self.probe()
        self.assertFalse(self.call('sent', leaseId=pending)['accepted'])
        self.call('finish', leaseId=pending)
        self.assertEqual(self.pair()['lastContactAt'], 10000)

    def test_disabled_plugin_still_accepts_inflight_completion(self):
        pair, _ = self.probe()
        lease = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+500)['leaseId']
        self.call('sent', leaseId=lease)
        self.store.enabled = False
        self.now += 10
        self.assertTrue(self.call('finish', leaseId=lease)['accepted'])
        self.assertEqual(self.pair()['inFlight'], 0)
        self.assertFalse(self.call('config')['enabled'])

    def test_private_material_shared_across_restart_never_visible_in_stats(self):
        claim = self.call('claim', demand=True)
        self.call('sent', leaseId=claim['leaseId'])
        material = {'state':'fake-state-secret', 'cookie':'fake-cookie-secret', 'expires':self.now+600}
        self.call('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=True, material=material)
        self.call('finish', leaseId=claim['leaseId'])
        self.pool = GatewayPool(self.temp.name, self.store, lambda:self.now)
        self.assertEqual(self.call('material', generation=claim['pair']['generation']), material)
        self.assertNotIn('fake-state-secret', json.dumps(self.pool.view()))
        self.assertNotIn('fake-cookie-secret', json.dumps(self.pool.view()))
        self.now += 601
        self.assertIsNone(self.call('material', generation=claim['pair']['generation']))

    def test_probe_interval_stays_integer_and_default_is_50ms(self):
        self.assertEqual(self.pool.settings_for_ui()['probeIntervalMs'], 50)
        self.assertIsInstance(self.pool.configure({'probeIntervalMs':50})['probeIntervalMs'], int)
        with self.assertRaises(ValueError): self.pool.configure({'probeIntervalMs':50.5})
        self.assertIsNone(self.call('plan', demand=False)['probe'])

    def test_error_attempts_remain_unclassified_and_failure_doubles_last_tier(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe(True)
        self.now += 3600
        claim = self.call('claim', demand=True)
        for retry in (0, 1):
            self.call('sent', leaseId=claim['leaseId'])
            self.now += 1
            self.call('probeAttempt', leaseId=claim['leaseId'], phase='replay', retry=retry, status=402, stateReturned=False, durationMs=42)
        result = self.call('probeResult', leaseId=claim['leaseId'], status=402, paired=True, gatewayMatches=True, failed=True)
        self.call('finish', leaseId=claim['leaseId'])
        self.assertIsNone(result['classification'])
        pair = self.pair()
        self.assertEqual(pair['retiredReason'], 'probe_failed')
        self.assertEqual(pair['restSeconds'], 7200)
        self.assertEqual(pair['restUntil'], self.now + 7200)
        attempts = [o for o in self.pool.view()['observations'] if o['source']=='probe-attempt']
        self.assertEqual([o['retry'] for o in attempts], [1,0])
        self.assertTrue(all(o['status']==402 and o['classification'] is None for o in attempts))

    def test_bootstrap_returning_state_is_material_acquisition_not_29(self):
        claim = self.call('claim', demand=True)
        self.call('sent', leaseId=claim['leaseId'])
        self.call('probeAttempt', leaseId=claim['leaseId'], phase='bootstrap', retry=0, status=200, stateReturned=True)
        sample = self.pool.view()['observations'][0]
        self.assertIsNone(sample['classification'])
        self.assertTrue(sample['stateReturned'])
        self.assertEqual(self.pair()['state'], 'preparing')

    def test_probe_finishes_after_disable_without_leaving_preparing_state(self):
        claim = self.call('claim', demand=True)
        self.call('sent', leaseId=claim['leaseId'])
        self.store.enabled = False
        self.now += 10
        self.call('finish', leaseId=claim['leaseId'])
        pair = self.pair()
        self.assertEqual(pair['state'],'cooling')
        self.assertEqual(pair['inFlight'],0)
        self.assertEqual(pair['restUntil'],self.now+3600)
        self.store.enabled = True
        self.assertIsNotNone(self.call('plan', demand=True)['probe'])

    def test_removed_gateway_cannot_publish_ready_or_send_reserved_request(self):
        pair,_ = self.probe()
        lease = self.call('reserve', gateway=pair['gateway'], generation=pair['generation'], deadline=self.now+500)['leaseId']
        self.pool.configure({'gateways':['unified-143']})
        self.assertFalse(self.call('sent', leaseId=lease)['accepted'])
        self.call('finish', leaseId=lease)
        claim = self.call('claim', demand=True)
        self.call('sent', leaseId=claim['leaseId'])
        self.pool.configure({'gateways':['unified-142']})
        self.call('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=True)
        self.call('finish', leaseId=claim['leaseId'])
        self.assertEqual(self.pair(gateway='unified-143')['state'],'cooling')

    def test_switch_records_follow_account_and_gateway_filters(self):
        self.probe()
        self.now += 181
        self.probe()
        self.assertEqual(len(self.pool.view(account='B')['switches']),0)
        self.assertEqual(len(self.pool.view(gateway='unified-143')['switches']),1)


if __name__ == '__main__':
    unittest.main()

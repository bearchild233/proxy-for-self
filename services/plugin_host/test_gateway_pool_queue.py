import unittest
from concurrent.futures import ThreadPoolExecutor

from gateway_pool import GatewayPool
import test_gateway_pool_history as history_tests


class GatewayPoolQueueTests(unittest.TestCase):
    setUp=history_tests.GatewayPoolHistoryTests.setUp
    call=history_tests.GatewayPoolHistoryTests.call
    send=history_tests.GatewayPoolHistoryTests.send
    report=history_tests.GatewayPoolHistoryTests.report
    probe=history_tests.GatewayPoolHistoryTests.probe
    business=history_tests.GatewayPoolHistoryTests.business
    finish=history_tests.GatewayPoolHistoryTests.finish
    retire=history_tests.GatewayPoolHistoryTests.retire

    def queue(self):
        return self.pool.view()['accountQueue']

    def register(self):
        return self.pool.runtime({'action':'active','candidates':[{'id':a,'exhausted':False} for a in ['A','B','C']]})

    def order(self):
        return [a['accountId'] for a in self.queue()['accounts']]

    def test_manual_order_persists_and_key_scope_stays_restricted(self):
        self.assertEqual(self.register()['accounts'],['A','B','C'])
        self.pool.update_queue({'revision':self.queue()['revision'],'accountIds':['C','A','B']})
        self.pool=GatewayPool(self.temp.name,self.store,lambda:self.now)
        self.assertEqual(self.order(),['C','A','B'])
        result=self.pool.runtime({'action':'active','candidates':[{'id':'A','exhausted':False},{'id':'C','exhausted':False}]})
        self.assertEqual(result['accounts'],['C','A'])

    def test_disable_stops_new_requests_but_keeps_sent_lease(self):
        self.register();pair=self.probe()
        sent,access=self.business(pair)
        unsent=self.call('reserve',gateway=pair['gateway'],generation=pair['generation'],deadline=self.now+100)['leaseId']
        self.pool.update_queue({'revision':self.queue()['revision'],'accountId':'A','enabled':False})
        self.assertFalse(self.call('sent',leaseId=unsent)['accepted'])
        self.assertFalse(self.call('reserve',gateway=pair['gateway'],generation=pair['generation'],deadline=self.now+100)['reserved'])
        self.assertFalse(self.call('claim',demand=True)['claimed'])
        self.assertTrue(self.call('plan',demand=True)['yieldAccount'])
        self.finish(sent,access)
        self.call('finish',leaseId=unsent)
        with self.pool.db(readonly=True) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM leases').fetchone()[0],0)
        self.assertEqual(self.order(),['A','B','C'])
        self.assertEqual(self.register()['accounts'],['B','C'])

    def test_full_gateway_round_moves_once_and_stale_probe_cannot_restart_it(self):
        self.register()
        turn=self.call('plan',demand=True)['queueTurn']
        first=self.probe();self.retire(first)
        self.assertEqual(self.order(),['A','B','C'])
        second=self.probe();self.now+=181
        self.assertTrue(self.call('plan',demand=True)['yieldAccount'])
        self.assertEqual(self.order(),['B','C','A'])
        self.assertFalse(self.call('claim',demand=True,queueTurn=turn)['claimed'])
        self.call('finish',leaseId=second['generation'])
        self.assertEqual(self.order(),['B','C','A'])

    def test_quota_moves_once_recovers_at_tail_and_cas_rejects_stale_drag(self):
        self.register();old=self.queue()['revision']
        self.call('quota',exhausted=True)
        self.assertEqual(self.order(),['B','C','A'])
        revision=self.queue()['revision']
        self.call('quota',exhausted=True)
        self.assertEqual(revision,self.queue()['revision'])
        with self.assertRaises(ValueError):
            self.pool.update_queue({'revision':old,'accountIds':['C','B','A']})
        self.call('quota',exhausted=False)
        self.assertEqual(self.order(),['B','C','A'])
        self.assertEqual(self.register()['accounts'],['B','C','A'])

    def test_reordered_account_can_prepare_without_interrupting_old_inflight(self):
        self.register();pair=self.probe();lease,access=self.business(pair)
        self.pool.update_queue({'revision':self.queue()['revision'],'accountIds':['B','A','C']})
        claim=self.call('claim',accountId='B',demand=True)
        self.assertTrue(claim['claimed'])
        self.assertEqual(self.pool.view(account='A')['pairs'][0]['state'],'draining')
        self.finish(lease,access)
        self.assertEqual(self.pool.view(account='A')['pairs'][0]['inFlight'],0)

    def test_simultaneous_quota_reports_only_move_once(self):
        self.register()
        revision=self.queue()['revision']
        with ThreadPoolExecutor(max_workers=2) as executor:
            list(executor.map(lambda _:self.call('quota',exhausted=True),range(2)))
        self.assertEqual(self.queue()['revision'],revision+1)
        self.assertEqual(self.order(),['B','C','A'])

    def test_unsent_failure_does_not_consume_gateway_round(self):
        self.register()
        for _ in range(3):
            claim=self.call('claim',demand=True)
            self.call('probeResult',leaseId=claim['leaseId'],failed=True)
            self.call('finish',leaseId=claim['leaseId'])
            self.now+=6
        self.assertEqual(self.order(),['A','B','C'])

    def test_disable_between_bootstrap_and_replay_stops_next_access(self):
        self.register()
        claim=self.call('claim',demand=True)
        self.send(claim['leaseId'],'bootstrap')
        self.pool.update_queue({'revision':self.queue()['revision'],'accountId':'A','enabled':False})
        self.assertFalse(self.call('sent',leaseId=claim['leaseId'],purpose='replay')['accepted'])
        self.call('probeResult',leaseId=claim['leaseId'],status=200,paired=True,gatewayMatches=True)
        self.call('finish',leaseId=claim['leaseId'])
        self.assertNotEqual(self.pool.view()['pairs'][0]['state'],'ready')

    def test_gateway_configuration_restarts_round_without_removed_candidates(self):
        self.register();self.retire(self.probe())
        self.pool.configure({'gateways':['unified-142']})
        self.now+=10
        self.probe();self.now+=181
        self.assertTrue(self.call('plan',demand=True)['yieldAccount'])
        self.assertEqual(self.order(),['B','C','A'])

    def test_all_cooling_skips_account_without_probing_and_restores_after_cooldown(self):
        self.pool.configure({'initialRestSeconds':7200,'maximumRestSeconds':14400})
        self.register()
        self.probe('29');self.probe('29')
        before=self.pool.view()['observations']
        self.assertEqual(self.register()['accounts'],['B','C'])
        self.assertEqual(self.pool.view()['observations'],before)
        self.now+=7201
        self.assertEqual(self.register()['accounts'],['B','C','A'])
        self.assertEqual(self.pool.view()['observations'],before)

    def test_selection_finishes_last_window_before_returning_queue_order(self):
        self.register();self.retire(self.probe());self.probe();self.now+=181
        self.assertEqual(self.register()['accounts'],['B','C','A'])
        revision=self.queue()['revision']
        self.register()
        self.assertEqual(self.queue()['revision'],revision)

    def test_other_keys_earlier_window_does_not_leave_candidate_waiting(self):
        self.register();self.probe()
        result=self.pool.runtime({'action':'active','candidates':[{'id':'B','exhausted':False}]})
        self.assertEqual(result['accounts'],[])
        self.pool.update_queue({'revision':self.queue()['revision'],'accountIds':['B','A','C']})
        result=self.pool.runtime({'action':'active','candidates':[{'id':'B','exhausted':False}]})
        self.assertEqual(result['accounts'],['B'])


if __name__ == '__main__':
    unittest.main()

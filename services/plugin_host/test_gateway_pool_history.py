import json
import tempfile
import unittest
from unittest.mock import patch
from concurrent.futures import ThreadPoolExecutor

from gateway_pool import GatewayPool
from test_gateway_pool import Store


class GatewayPoolHistoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.now = 10000.0
        self.store = Store(self.temp.name)
        self.pool = GatewayPool(self.temp.name, self.store, lambda:self.now)
        self.pool.configure({'gateways':['unified-142','unified-143'],'initialRestSeconds':1,'maximumRestSeconds':8})
        self.sequence = 0

    def call(self, action, **data):
        return self.pool.runtime({'action':action,'accountId':'A',**data})

    def send(self, lease, kind):
        self.sequence += 1
        access = {'id':str(self.sequence),'startedAt':self.now,'elapsedMs':0,'ended':False,'outcome':'headers'}
        self.call('sent',leaseId=lease,timingVersion=2,accessId=access['id'],purpose=kind)
        return access

    def report(self, action, lease, access, end=True, **extra):
        item = {**access,'elapsedMs':(self.now-access['startedAt'])*1000,'ended':end,'outcome':'completed' if end else 'headers'}
        return self.call(action,leaseId=lease,access=item,**extra)

    def probe(self, verdict='21', **extra):
        claim = self.call('claim',demand=True,**extra)
        self.assertTrue(claim['claimed'])
        lease = claim['leaseId'];start=self.now
        for phase in ('bootstrap','replay'):
            access = self.send(lease,phase);self.now += 0.1
            self.report('probeAttempt',lease,access,phase=phase,retry=0,status=200,stateReturned=phase=='bootstrap' or verdict=='29')
        self.call('probeResult',leaseId=lease,status=200,stateReturned=verdict=='29',paired=True,gatewayMatches=True,
                  preparationStartedAt=start,durationMs=(self.now-start)*1000)
        self.call('finish',leaseId=lease)
        return claim['pair']

    def business(self, pair):
        lease = self.call('reserve',gateway=pair['gateway'],generation=pair['generation'],deadline=self.now+20000)['leaseId']
        return lease,self.send(lease,'business')

    def finish(self, lease, access, delay=10):
        self.now += delay
        self.report('finish',lease,access)

    def retire(self, pair):
        lease = self.call('reserve',gateway=pair['gateway'],generation=pair['generation'],deadline=self.now+100)['leaseId']
        self.call('invalidate',leaseId=lease)
        self.call('finish',leaseId=lease)

    def stats(self, **query):
        return self.pool.statistics(query)

    def test_headers_do_not_finish_business_and_overlap_is_merged(self):
        pair=self.probe();lease,a=self.business(pair)
        self.now+=1
        self.report('businessResult',lease,a,end=False,status=200,stateReturned=False,paired=True,gatewayMatches=True)
        lease2,b=self.business(pair)
        self.finish(lease,a,1);self.finish(lease2,b,1)
        row=self.stats()['records'][0]
        self.assertAlmostEqual(row['businessSeconds'],3)
        self.assertAlmostEqual(row['requestSeconds'],4)
        self.assertEqual(row['requests'],2)
        self.assertEqual(len(row['segments']),1)
        self.assertFalse(row['incomplete'])

    def test_ticket_is_separate_and_finish_timestamp_excludes_rpc_delay(self):
        pair=self.probe()
        lease=self.call('reserve',gateway=pair['gateway'],generation=pair['generation'],deadline=self.now+100)['leaseId']
        ticket=self.send(lease,'ticket');self.now+=3
        self.report('ticketResult',lease,ticket,model='gpt-6-astra',status=200)
        business=self.send(lease,'business');self.now+=5
        ended={**business,'elapsedMs':5000,'ended':True,'outcome':'completed'}
        self.now+=7
        self.call('finish',leaseId=lease,access=ended)
        row=self.stats()['records'][0]
        self.assertAlmostEqual(row['businessSeconds'],5)
        self.assertAlmostEqual(row['ticketSeconds'],3)

    def test_two_hour_boundary_persists_segments_without_retiring_window(self):
        pair=self.probe();lease,a=self.business(pair);self.finish(lease,a)
        self.now+=7199
        lease,a=self.business(pair);self.finish(lease,a)
        self.assertEqual(len(self.stats()['records'][0]['segments']),1)
        last=self.now;self.now+=7200
        self.stats()
        with self.pool.db(readonly=True) as db:
            segment=db.execute('SELECT * FROM gp_segments').fetchone()
            self.assertEqual(segment['closed'],last+7200)
            self.assertEqual(segment['last_end'],last)
        lease,a=self.business(pair);self.finish(lease,a)
        row=self.stats()['records'][0]
        self.assertEqual(len(row['segments']),2)
        self.assertIsNone(row['retiredAt'])

    def test_inflight_prevents_idle_closure(self):
        pair=self.probe();lease,a=self.business(pair)
        self.now+=7300
        self.assertEqual(self.stats()['records'][0]['segments'],[])
        self.finish(lease,a,1)
        self.assertEqual(len(self.stats()['records'][0]['segments']),1)
        self.assertIsNone(self.stats()['records'][0]['segments'][0]['closedAt'])

    def test_repeat_candidate_stays_in_same_batch_and_restart_keeps_number(self):
        self.probe('29');self.now+=2
        repeated=self.probe('29',catalog=[{'gateway':'unified-142','validForS':600}])
        self.assertEqual(repeated['gateway'],'unified-142')
        batch=self.stats(view='summary')['batches'][0]
        self.assertEqual(batch['number'],1)
        self.assertEqual(batch['status'],'active')
        self.assertEqual(len(batch['candidates']),2)
        self.pool=GatewayPool(self.temp.name,self.store,lambda:self.now)
        self.now+=5
        self.probe('29',catalog=[{'gateway':'unified-143','validForS':600}])
        self.assertEqual(self.stats(view='summary')['batches'][0]['status'],'complete')
        self.now+=10
        self.probe('29',catalog=[{'gateway':'unified-142','validForS':600},{'gateway':'unified-143','validForS':600}])
        self.assertEqual(self.stats(view='summary')['batches'][0]['number'],2)

    def test_retirement_drains_before_batch_and_cooling(self):
        pair=self.probe();lease,a=self.business(pair)
        self.now+=181
        self.probe('29')
        self.retire(pair)
        self.assertEqual(self.stats(view='summary')['batches'][0]['status'],'active')
        self.finish(lease,a,3)
        self.assertEqual(self.stats(view='summary')['batches'][0]['status'],'complete')
        row=next(r for r in self.stats()['records'] if r['id']==pair['generation'])
        self.assertGreater(row['drainedAt'],row['retiredAt'])

    def test_recovery_requires_actual_21_and_records_each_tier(self):
        self.probe('29');self.now+=2
        cycle=self.stats(view='cooling')['recoveries'][0]
        self.assertEqual(cycle['state'],'due')
        self.assertIsNone(cycle['recoveredAt'])
        self.probe('29',catalog=[{'gateway':'unified-142','validForS':600}]);self.now+=3
        self.probe('21',catalog=[{'gateway':'unified-142','validForS':600}])
        cycle=self.stats(view='cooling')['recoveries'][0]
        self.assertEqual(cycle['state'],'recovered')
        self.assertEqual([s['plannedSeconds'] for s in cycle['steps']],[1,2])
        self.assertEqual([s['result'] for s in cycle['steps']],['29','21'])

    def test_no_contact_failure_does_not_create_cooling_or_complete_candidate(self):
        claim=self.call('claim',demand=True)
        self.call('probeResult',leaseId=claim['leaseId'],failed=True)
        self.call('finish',leaseId=claim['leaseId'])
        self.assertEqual(self.stats(view='cooling')['recoveries'],[])
        self.assertEqual(self.stats(view='summary')['batches'][0]['processed'],{})

    def test_recovery_learning_and_no_contact_retry_keep_original_cooling(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe('29');self.now+=2
        self.probe('21');self.retire(self.pool.view()['pairs'][0]);self.now+=2
        claim=self.call('claim',demand=True)
        self.call('probeResult',leaseId=claim['leaseId'],failed=True)
        self.call('finish',leaseId=claim['leaseId'])
        cycles=self.stats(view='cooling')['recoveries']
        self.assertEqual(len(cycles),2)
        self.assertEqual(len(cycles[0]['steps']),1)
        self.now+=6
        self.probe('21')
        cycle=self.stats(view='cooling')['recoveries'][0]
        self.assertEqual(cycle['steps'][-1]['learnedRestSeconds'],1)

    def test_legacy_and_expired_access_are_unknown_not_infinite_duration(self):
        pair=self.probe();lease,a=self.business(pair)
        self.now+=20001
        self.call('active')
        row=self.stats()['records'][0]
        self.assertIsNone(row['businessSeconds'])
        self.assertTrue(row['incomplete'])
        self.assertEqual(row['knownBusinessSeconds'],0)

    def test_duplicate_finish_is_idempotent_and_filters_keep_full_totals(self):
        pair=self.probe();lease,a=self.business(pair);start=self.now;self.finish(lease,a)
        self.report('finish',lease,a)
        row=self.stats(**{'from':start+2,'to':start+7})['records'][0]
        self.assertAlmostEqual(row['businessSeconds'],5)
        self.assertEqual(row['requests'],0)
        self.assertTrue(row['partial'])
        self.assertEqual(self.stats(accountId='missing')['page']['total'],0)
        self.assertEqual(self.stats(gateway='unified-143')['summary']['businessSeconds'],0)

    def test_configuration_interrupts_and_history_failure_does_not_break_finish(self):
        pair=self.probe();lease,a=self.business(pair)
        def fail(*_): raise RuntimeError('test')
        with patch('gateway_pool_history.sync',new=fail):
            self.finish(lease,a)
        with self.pool.db(readonly=True) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM leases').fetchone()[0],0)
        self.assertGreater(self.stats()['collectionGaps'],0)
        self.pool.configure({'gateways':['unified-142']})
        self.assertEqual(self.stats(view='summary')['batches'][0]['status'],'interrupted')

    def test_read_snapshot_does_not_block_runtime_writer(self):
        pair=self.probe()
        with self.pool.db(readonly=True) as db:
            db.execute('SELECT COUNT(*) FROM gp_windows').fetchone()
            with ThreadPoolExecutor(max_workers=1) as executor:
                result=executor.submit(self.business,pair).result(timeout=2)
            self.assertTrue(result[0])

    def test_invalid_queries_rejected_and_private_material_not_exposed(self):
        self.probe()
        for query in ({'from':float('nan')},{'page':True},{'pageSize':101},{'view':'secret'}):
            with self.assertRaises(ValueError):self.stats(**query)
        value=json.dumps(self.stats())
        for field in ('cookie','poolToken','material','turn-state'):
            self.assertNotIn(field,value)

    def test_card_read_after_due_is_passive_and_waits_for_actual_visit(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe('29');last_end=self.now;self.now+=10
        with self.pool.db(readonly=True) as db:
            before=[tuple(row) for row in db.execute('SELECT * FROM pairs')]
        with patch.object(self.pool,'runtime',side_effect=AssertionError('view must not trigger runtime')):
            summary=self.pool.view()['pairs'][0]['coolingSummary']
        self.assertEqual(summary['visits'],[])
        self.assertIsNone(summary['current']['probeAt'])
        self.assertAlmostEqual(summary['current']['startedAt'],last_end)
        with self.pool.db(readonly=True) as db:
            self.assertEqual(before,[tuple(row) for row in db.execute('SELECT * FROM pairs')])
            self.assertEqual(db.execute('SELECT COUNT(*) FROM leases').fetchone()[0],0)
        self.probe('21')
        summary=self.pool.view()['pairs'][0]['coolingSummary']
        self.assertIsNone(summary['current'])
        self.assertAlmostEqual(summary['visits'][0]['idleSeconds'],10)
        self.assertAlmostEqual(summary['visits'][0]['afterDueSeconds'],9)
        self.assertEqual(summary['visits'][0]['result'],'21')

    def test_card_recent_visits_keep_account_scope_and_latest_failed_attempt(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe('29')
        for _ in range(7):
            self.now+=10
            self.probe('29')
        summary=self.pool.view(account='A')['pairs'][0]['coolingSummary']
        self.assertEqual(len(summary['visits']),5)
        self.assertTrue(summary['hasMore'])
        self.assertEqual(summary['visits'][0]['result'],'29')
        self.assertIsNone(summary['current']['probeAt'])
        self.assertEqual(summary['current']['plannedSeconds'],8)
        self.assertGreater(summary['visits'][0]['probeAt'],summary['visits'][-1]['probeAt'])
        self.assertEqual(self.pool.view(account='B')['pairs'],[])

    def test_account_cooling_analysis_uses_all_visits_and_actual_interval_bins(self):
        self.pool.configure({'gateways':['unified-142']})
        self.probe('29')
        for _ in range(7):
            self.now+=7201
            self.probe('29')
        self.now+=14401
        self.probe('21')
        with patch.object(self.pool,'runtime',side_effect=AssertionError('statistics must be passive')):
            summary=self.stats(view='summary',accountId='A',pageSize=1)['coolingAnalysis']
        self.assertEqual(summary['total'],8)
        self.assertEqual(summary['gateways'],1)
        self.assertEqual(summary['unrecovered'],7)
        self.assertEqual(summary['recovered'],1)
        self.assertEqual(summary['bins'][1]['unrecovered'],7)
        self.assertEqual(summary['bins'][2]['recovered'],1)
        self.assertAlmostEqual(summary['recoveredMedianSeconds'],14401)
        self.assertEqual(self.stats(view='summary',accountId='B')['coolingAnalysis']['total'],0)
        self.assertEqual(self.stats(view='summary',gateway='unified-143')['coolingAnalysis']['total'],0)

    def test_cooling_analysis_filters_visit_time_and_separates_unknown_outcomes(self):
        self.now=200000
        # 周期开始在筛选范围外，只有实际再访问起点决定样本归属。
        steps=[]
        for probe,idle,result in [(100000,7199,'21'),(100001,7200,'29'),(100002,14400,'failed'),
                                  (100003,28800,None),(100004,None,'21'),(100005,57600,'21')]:
            steps.append(dict(probeAt=probe,startedAt=probe-idle if idle is not None else None,
                              result=result,resultAt=probe+0.5 if result else None))
        with self.pool.db() as db:
            db.execute('INSERT INTO gp_cooling(account,gateway,started,recovered,data) VALUES (?,?,?,?,?)',
                       ('A','unified-142',10000,None,json.dumps({'steps':steps})))
        data=self.stats(view='summary',accountId='A',**{'from':100000,'to':100005})['coolingAnalysis']
        self.assertEqual(data['total'],5)
        self.assertEqual(data['missingInterval'],1)
        self.assertEqual(data['failed'],1)
        self.assertEqual(data['pending'],1)
        self.assertEqual([b['recovered'] for b in data['bins']],[1,0,0,0,0])
        self.assertEqual(data['bins'][1]['unrecovered'],1)
        self.assertEqual(data['bins'][2]['failed'],1)
        self.assertEqual(data['bins'][3]['pending'],1)
        self.assertEqual(data['recoveredMedianSeconds'],7199)
        future=self.stats(view='summary',**{'from':100005,'to':100005.25})['coolingAnalysis']
        self.assertEqual(future['pending'],1)
        self.assertEqual(future['recovered'],0)
        end=self.stats(view='summary',**{'from':100005,'to':100006})['coolingAnalysis']
        self.assertEqual(end['bins'][4]['recovered'],1)

    def test_rotation_detail_keeps_detection_ticket_and_business_in_exact_generation(self):
        self.pool.configure({'gateways':['unified-142']})
        first=self.probe()
        lease=self.call('reserve',gateway=first['gateway'],generation=first['generation'],deadline=self.now+100)['leaseId']
        ticket=self.send(lease,'ticket');self.now+=0.2
        self.report('ticketResult',lease,ticket,model='gpt-6-astra',status=200,stateReturned=True)
        business=self.send(lease,'business');self.now+=0.2
        self.report('businessResult',lease,business,end=False,status=200,stateReturned=False,paired=True,gatewayMatches=True)
        self.finish(lease,business)
        self.retire(first);self.now+=10
        second=self.probe()
        rows={r['id']:r for r in self.stats()['records']}
        self.assertEqual(rows[first['generation']]['probeResult'],'21')
        self.assertEqual(rows[first['generation']]['observationCount'],5)
        self.assertEqual({o['source'] for o in rows[first['generation']]['observations']},{'probe-attempt','probe','ticket','business'})
        self.assertEqual(rows[second['generation']]['observationCount'],3)
        for generation,row in rows.items():
            self.assertTrue(all(o['generation']==generation for o in row['observations']))
        self.assertEqual(self.stats(accountId='B')['records'],[])

    def test_rotation_detail_limits_visible_detection_rows_but_keeps_total(self):
        pair=self.probe()
        with self.pool.db() as db:
            sample=db.execute('SELECT * FROM observations ORDER BY id LIMIT 1').fetchone()
            for _ in range(102):
                db.execute('INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)',
                           (sample['at'],sample['account'],sample['gateway'],sample['data']))
            legacy=json.loads(sample['data']);legacy.pop('generation')
            db.execute('INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)',
                       (sample['at'],sample['account'],sample['gateway'],json.dumps(legacy)))
        row=next(r for r in self.stats()['records'] if r['id']==pair['generation'])
        self.assertEqual(row['observationCount'],105)
        self.assertEqual(len(row['observations']),100)
        self.assertGreater(row['observations'][0]['id'],row['observations'][-1]['id'])


if __name__ == '__main__':
    unittest.main()

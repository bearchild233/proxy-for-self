import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
from slots import SlotController, SystemdCaddyDriver, atomic_json

class Driver:
    def __init__(self):
        self.active='a';self.alive={'a'};self.events=[];self.failure=None
    def routed_slot(self):return self.active
    def healthy(self,s):return s in self.alive
    def running(self,s):return s in self.alive
    def activate(self,s):self.events.append(('activate',s))
    def before_switch(self,old,new):
        if self.failure=='memory':raise RuntimeError('memory pressure')
    def prepare(self,s,r):self.events.append(('prepare',s))
    def start(self,s):
        self.events.append(('start',s))
        if self.failure=='start':raise RuntimeError('bad binary')
        self.alive.add(s)
    def verify(self,s,ingress=False):
        self.events.append(('verify-public' if ingress else 'verify',s))
        if self.failure==('public' if ingress else 'ready') and s=='b':raise RuntimeError('not healthy')
    def switch(self,old,new):
        assert self.active==old and new in self.alive
        self.events.append(('switch',new));self.active=new
    def stop(self,s):
        assert s!=self.active,'Never stop active slot'
        self.events.append(('stop',s))
        if self.failure=='drain' and s=='a':raise RuntimeError('still draining')
        self.alive.discard(s)

class SlotTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.path=Path(self.temp.name)/'state.json'
        atomic_json(self.path,{'active':'a','phase':'idle'})
        self.driver=Driver();self.controller=SlotController(self.path,self.driver)
    def test_alternates_twice_and_stops_only_after_public_verification(self):
        self.controller.deploy('/releases/v2');self.controller.deploy('/releases/v3')
        self.assertEqual(self.driver.alive,{'a'})
        self.assertEqual([e for e in self.driver.events if e[0]=='switch'],[('switch','b'),('switch','a')])
        events=self.driver.events
        self.assertLess(events.index(('verify-public','b')),events.index(('stop','a')))
        self.assertLess(events.index(('verify-public','a')),events.index(('stop','b')))
    def test_candidate_start_or_health_failure_preserves_active(self):
        for failure in ('start','ready','memory'):
            with self.subTest(failure=failure):
                self.driver.failure=failure
                with self.assertRaises(RuntimeError):self.controller.deploy('/releases/bad')
                self.assertEqual(self.driver.alive,{'a'})
                self.assertNotIn(('stop','a'),self.driver.events)
                self.assertNotIn(('switch','b'),self.driver.events)
    def test_public_failure_switches_back_before_stopping_candidate(self):
        self.driver.failure='public'
        with self.assertRaises(RuntimeError):self.controller.deploy('/releases/bad')
        self.assertEqual(self.driver.alive,{'a'})
        self.assertLess(self.driver.events.index(('switch','a')),self.driver.events.index(('stop','b')))
    def test_drain_failure_keeps_new_slot_and_resume_only_retries_old_drain(self):
        self.driver.failure='drain'
        with self.assertRaises(RuntimeError):self.controller.deploy('/releases/v2')
        self.assertEqual(self.driver.active,'b')
        self.assertEqual(json.loads(self.path.read_text())['phase'],'draining')
        self.driver.failure=None;self.controller.resume()
        self.assertEqual(self.driver.alive,{'b'})
        self.assertNotIn(('stop','b'),self.driver.events)
    def test_resume_uncertain_switch_reads_ingress_and_restores_old(self):
        atomic_json(self.path,{'active':'a','phase':'switching','previous':'a','candidate':'b'})
        self.driver.active='b';self.driver.alive.add('b')
        self.controller.resume()
        self.assertEqual(self.driver.alive,{'a'})
    def test_running_inactive_slot_cannot_be_overwritten(self):
        self.driver.alive.add('b')
        with self.assertRaises(RuntimeError):self.controller.deploy('/releases/v2')
        self.assertEqual(self.driver.events,[])

    def test_failed_original_preserves_candidate_instead_of_stopping_last_healthy_process(self):
        atomic_json(self.path,{'active':'a','phase':'ready','previous':'a','candidate':'b'})
        self.driver.alive={'b'}
        with self.assertRaisesRegex(RuntimeError,'Original slot unhealthy'):
            self.controller.resume()
        self.assertEqual(self.driver.alive,{'b'})
        self.assertEqual(json.loads(self.path.read_text())['phase'],'attention')

    def test_missing_websocket_reload_protection_rejects_switch_before_patch(self):
        driver=object.__new__(SystemdCaddyDriver)
        driver.config={'caddy_dial_path':'/id/gateway/upstreams/0/dial','drain_timeout':660}
        driver.caddy=Mock(return_value=({'handler':'reverse_proxy'},'etag'))
        with self.assertRaisesRegex(RuntimeError,'stream_close_delay'):
            driver.switch('a','b')
        driver.caddy.assert_called_once_with(path='/id/gateway')

    def test_boot_starts_only_committed_slot_and_repairs_unconfirmed_route(self):
        atomic_json(self.path,{'active':'a','phase':'switching','previous':'a','candidate':'b'})
        self.driver.alive=set();self.driver.active='b'
        self.controller.boot()
        self.assertEqual(self.driver.alive,{'a'})
        self.assertEqual(self.driver.active,'a')
        self.assertNotIn(('start','b'),self.driver.events)

    def test_boot_after_committed_switch_starts_b_without_starting_a(self):
        atomic_json(self.path,{'active':'b','phase':'draining','previous':'a','candidate':'b'})
        self.driver.alive=set();self.driver.active='b'
        self.controller.boot()
        self.assertEqual(self.driver.alive,{'b'})
        self.assertNotIn(('start','a'),self.driver.events)

if __name__=='__main__':unittest.main()

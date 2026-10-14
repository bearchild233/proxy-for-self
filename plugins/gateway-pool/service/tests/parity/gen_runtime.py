#!/usr/bin/env python3.10
"""网关池运行状态机的一致性对照：用 Python 实现驱动动作序列，供 Rust 插件重放（见同目录 README.md）。
依赖 services/plugin_host 里的 Python 实现，P2 删除 Python 时随之删除。

用法：
  gen_runtime.py generate <仓库路径> <产物目录>      # 用 Python 实现驱动动作序列，记录每一步的返回值与结束时的整库内容
  gen_runtime.py verify-rust <仓库路径> <产物目录>   # Python 读取 Rust 写出的数据库，比较管理读取结果

Python 侧的 uuid4 被替换为确定的序列（000…001、000…002…），Rust 侧使用相同序列；时钟为注入的同一份浮点时间。
序列既有按功能命名的场景，也有固定种子的随机序列（覆盖异常输入、重启、停用与并发状态交错）。
"""
import json
import os
import random
import shutil
import sqlite3
import sys
import tempfile
import types
from pathlib import Path

REPO = Path(sys.argv[2] if len(sys.argv) > 2 else '.').resolve()
sys.path.insert(0, str(REPO / 'services/plugin_host'))

import gateway_pool as gp  # noqa: E402
from gateway_pool import GatewayPool  # noqa: E402
import test_gateway_pool as base  # noqa: E402  (策略文件加载用的 Store 桩)

START = 10000.0
TABLES = ['settings', 'pairs', 'leases', 'observations', 'switches', 'materials', 'catalogs', 'model_tickets',
          'group_events', 'recovery_checks', 'gp_history_meta', 'gp_batches', 'gp_windows', 'gp_accesses',
          'gp_segments', 'gp_cooling', 'gp_account_queue', 'gp_queue_revision']
JSON_COLUMNS = {'data', 'candidates', 'processed', 'remaining'}


class Ids:
    """替换 gateway_pool.uuid：每个序列从 1 开始，重启后继续计数。"""

    def __init__(self):
        self.count = 0

    def uuid4(self):
        self.count += 1
        return types.SimpleNamespace(hex='%032x' % self.count)


def dump_database(path):
    connection = sqlite3.connect(path)
    try:
        result = {}
        for table in TABLES:
            cursor = connection.execute('SELECT * FROM %s ORDER BY rowid' % table)
            names = [item[0] for item in cursor.description]
            rows = []
            for row in cursor.fetchall():
                entry = {}
                for name, value in zip(names, row):
                    if name in JSON_COLUMNS and isinstance(value, str):
                        value = json.loads(value)
                    entry[name] = value
                rows.append(entry)
            result[table] = rows
        return result
    finally:
        connection.close()


def py_runtime(pool, message):
    try:
        return {'ok': pool.runtime(message)}
    except ValueError as error:
        return {'error': str(error)}
    except PermissionError as error:
        return {'forbidden': str(error)}
    except (KeyError, TypeError):
        # Python 里是 KeyError/TypeError（宿主按 400 处理）；Rust 返回任意参数错误即可。
        return {'invalid': True}


def py_admin(pool, method, data):
    try:
        if method == 'configure':
            return {'ok': pool.configure(data)}
        if method == 'queue.update':
            return {'ok': pool.update_queue(data)}
        if method == 'read':
            value = pool.view(data.get('accountId'), data.get('gateway'), data.get('limit', 100), data.get('before'))
            value['settings'] = pool.settings_for_ui()
            return {'ok': value}
        if method == 'history.read':
            return {'ok': pool.statistics(data)}
        raise AssertionError('unknown method ' + method)
    except ValueError as error:
        return {'error': str(error)}
    except PermissionError as error:
        return {'forbidden': str(error)}


def clean(value):
    """把结果转成可 JSON 序列化的纯数据（深拷贝，避免后续修改）。"""
    return json.loads(json.dumps(value, allow_nan=False))


class Run:
    """一个动作序列：同时驱动 Python 实现并记录每一步的消息与返回值。"""

    def __init__(self, name):
        self.name = name
        self.root = Path(tempfile.mkdtemp(prefix='gp-runtime-'))
        self.clock = [START]
        self.ids = Ids()
        gp.uuid = self.ids
        self.store = base.Store(self.root)
        self.pool = GatewayPool(self.root, self.store, lambda: self.clock[0])
        self.steps = []
        self.enabled = True
        self.configure({'gateways': ['unified-142', 'unified-143'], 'initialRestSeconds': 3600})

    # ---- 时间 ----
    def advance(self, seconds):
        self.clock[0] = self.clock[0] + seconds

    @property
    def now(self):
        return self.clock[0]

    # ---- 记录 ----
    def _record(self, op, expect, **fields):
        self.steps.append({'op': op, 'clock': self.clock[0], 'expect': clean(expect), **clean(fields)})
        return expect

    def runtime(self, message):
        gp.uuid = self.ids
        return self._record('runtime', py_runtime(self.pool, message), message=message)

    def admin(self, method, data):
        gp.uuid = self.ids
        return self._record('admin', py_admin(self.pool, method, data), method=method, data=data)

    def restart(self):
        self.pool = GatewayPool(self.root, self.store, lambda: self.clock[0])
        self._record('restart', {'ok': None})

    def enable(self, flag):
        self.store.enabled = flag
        self.enabled = flag
        self._record('enable', {'ok': None}, flag=flag)

    # ---- 便捷调用 ----
    def rt(self, action, account='A', **data):
        """返回成功时的结果；出错时返回带 __error__ 的字典。"""
        result = self.runtime({'action': action, 'accountId': account, **data})
        if 'ok' in result:
            return result['ok']
        return {'__error__': result}

    def configure(self, data):
        return self.admin('configure', data)

    def read(self, **data):
        return self.admin('read', data)

    def stats(self, **data):
        return self.admin('history.read', data)

    def revision(self):
        return self.read()['ok']['accountQueue']['revision']

    def reorder(self, accounts, revision=None):
        return self.admin('queue.update', {'revision': self.revision() if revision is None else revision, 'accountIds': accounts})

    def toggle(self, account, enabled):
        return self.admin('queue.update', {'revision': self.revision(), 'accountId': account, 'enabled': enabled})

    def register(self, accounts=('A', 'B', 'C'), exhausted=()):
        return self.runtime({'action': 'active', 'candidates': [{'id': a, 'exhausted': a in exhausted} for a in accounts]})

    # ---- 常用流程 ----
    def timed(self, kind):
        self.seq = getattr(self, 'seq', 0) + 1
        return {'id': 'x%d' % self.seq, 'startedAt': self.now, 'kind': kind}

    def send(self, lease, kind, account='A', timing=True):
        if not timing:
            return self.rt('sent', account, leaseId=lease)
        access = self.timed(kind)
        self.rt('sent', account, leaseId=lease, timingVersion=2, accessId=access['id'], purpose=kind)
        return access

    def access(self, access, end=True, elapsed=None):
        started = access['startedAt']
        millis = (self.now - started) * 1000 if elapsed is None else elapsed
        return {'id': access['id'], 'startedAt': started, 'elapsedMs': millis, 'ended': end,
                'outcome': 'completed' if end else 'headers'}

    def probe(self, account='A', verdict='21', material=None, timing=True, demand=True, catalog=None, finish=True, **extra):
        message = {'demand': demand}
        if catalog is not None:
            message['catalog'] = catalog
        claim = self.rt('claim', account, **message)
        if not claim.get('claimed'):
            return claim
        lease, start = claim['leaseId'], self.now
        if timing:
            for phase in ('bootstrap', 'replay'):
                access = self.send(lease, phase, account)
                self.advance(0.1)
                self.rt('probeAttempt', account, leaseId=lease, access=self.access(access), phase=phase, retry=0,
                        status=200, stateReturned=phase == 'bootstrap' or verdict == '29')
        else:
            self.send(lease, 'x', account, timing=False)
        data = dict(leaseId=lease, status=200, stateReturned=verdict == '29', paired=True, gatewayMatches=True,
                    preparationStartedAt=start, durationMs=(self.now - start) * 1000)
        if material is None and verdict == '21':
            material = {'state': 'st-%s' % lease[-4:], 'cookie': 'ck-%s' % lease[-4:], 'expires': self.now + 900}
        if material:
            data['material'] = material
        data.update(extra)
        self.rt('probeResult', account, **data)
        if finish:
            self.rt('finish', account, leaseId=lease)
        return claim

    def reserve(self, claim, account='A', ahead=200):
        pair = claim['pair']
        return self.rt('reserve', account, gateway=pair['gateway'], generation=pair['generation'], deadline=self.now + ahead)

    def business(self, claim, account='A', timing=True):
        reserved = self.reserve(claim, account)
        lease = reserved['leaseId']
        access = self.send(lease, 'business', account, timing)
        return lease, access

    def end_business(self, lease, access, account='A', delay=5, verdict=None):
        self.advance(delay)
        if verdict is not None:
            self.rt('businessResult', account, leaseId=lease, status=200, stateReturned=verdict == '29', paired=True,
                    gatewayMatches=True, access=self.access(access, end=False) if access else None)
        self.rt('finish', account, leaseId=lease, access=self.access(access) if access else None)

    def snapshot(self):
        """结束时的统一读取：页面读取、各类统计，然后是整库内容。"""
        if self.enabled:
            for query in [{}, {'accountId': 'A'}, {'accountId': 'B', 'limit': 5}, {'gateway': 'unified-142', 'limit': 3}]:
                self.read(**query)
            for query in [{}, {'view': 'summary'}, {'view': 'cooling'}, {'view': 'records', 'pageSize': 2, 'page': 1},
                          {'accountId': 'A'}, {'view': 'summary', 'accountId': 'A'}, {'from': START, 'to': self.now}]:
                self.stats(**query)
        checkpoint = sqlite3.connect(self.pool.path, isolation_level=None)
        try:
            checkpoint.execute('PRAGMA wal_checkpoint(TRUNCATE)')
        finally:
            checkpoint.close()
        database = dump_database(self.pool.path)
        document = {'name': self.name, 'steps': self.steps, 'database': clean(database), 'finalClock': self.now}
        shutil.rmtree(self.root, ignore_errors=True)
        return document


# ---------------------------------------------------------------- 命名场景

def scenario_lazy_ticket_success():
    run = Run('lazy-on-demand-ticket-21')
    run.rt('plan', demand=False)
    run.rt('plan', demand=True)
    run.rt('config')
    claim = run.probe()
    pair = claim['pair']
    run.rt('material', generation=pair['generation'])
    run.rt('plan', demand=True)
    lease, access = run.business(claim)
    ticket = {'state': 'astra-state', 'cookie': 'ck', 'expires': run.now + 600}
    run.rt('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, stateReturned=True, durationMs=5, material=ticket,
           access=run.access(access, end=False))
    run.rt('ticket', generation=pair['generation'], model='gpt-6-astra')
    run.rt('ticket', generation=pair['generation'], model='gpt-6.1-sol')
    run.read()
    run.end_business(lease, access, verdict='21')
    run.rt('active')
    run.rt('active', candidates=[{'id': 'A', 'exhausted': False}])
    run.advance(700)
    run.rt('material', generation=pair['generation'])
    run.rt('ticket', generation=pair['generation'], model='gpt-6-astra')
    return run.snapshot()


def scenario_ticket_29_backoff():
    run = Run('ticket-29-backoff')
    run.configure({'gateways': ['unified-142'], 'initialRestSeconds': 60, 'maximumRestSeconds': 480})
    for gap in (0, 61, 121, 241, 481, 481):
        run.advance(gap)
        run.probe(verdict='29')
        run.rt('plan', demand=True)
    run.advance(500)
    run.probe(verdict='21')
    run.advance(100)
    run.probe(verdict='21')
    return run.snapshot()


def scenario_ticket_failures():
    run = Run('ticket-failure')
    run.configure({'gateways': ['unified-142'], 'initialRestSeconds': 10, 'maximumRestSeconds': 80})
    claim = run.rt('claim', demand=True)
    run.rt('probeResult', leaseId=claim['leaseId'], failed=True)
    run.rt('finish', leaseId=claim['leaseId'])
    run.advance(6)
    claim = run.rt('claim', demand=True)
    lease = claim['leaseId']
    run.send(lease, 'bootstrap')
    run.advance(1)
    run.rt('probeAttempt', leaseId=lease, phase='replay', retry=1, status=402, stateReturned=False, durationMs=40)
    run.rt('probeResult', leaseId=lease, status=402, paired=True, gatewayMatches=True, failed=True)
    run.rt('finish', leaseId=lease)
    run.advance(11)
    claim = run.rt('claim', demand=True)
    lease = claim['leaseId']
    run.send(lease, 'replay')
    run.advance(1)
    run.rt('probeResult', leaseId=lease, status=401, paired=True, gatewayMatches=True)
    run.rt('finish', leaseId=lease)
    run.advance(31)
    claim = run.rt('claim', demand=True)
    run.rt('sent', leaseId=claim['leaseId'])
    run.rt('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=False)
    run.rt('finish', leaseId=claim['leaseId'])
    # 错误输入：未发送的判定、租约类型不符、探测访问记录无效、材料无效。
    run.advance(100)
    claim = run.rt('claim', demand=True)
    lease = claim['leaseId']
    run.rt('probeResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True)
    run.rt('businessResult', leaseId=lease, status=200)
    run.rt('probeAttempt', leaseId=lease, phase='replay', retry=0)
    run.rt('sent', leaseId=lease)
    run.rt('probeAttempt', leaseId=lease, phase='other', retry=0)
    run.rt('probeAttempt', leaseId=lease, phase='replay', retry=2)
    run.rt('probeAttempt', leaseId=lease, phase='replay', retry=1.0, status=500)
    run.rt('probeResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True,
           material={'state': 'a', 'cookie': 'b'})
    run.rt('probeResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True,
           material={'state': 'a\nb', 'cookie': 'b', 'expires': run.now + 10})
    run.rt('probeResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True,
           material={'state': 'a', 'cookie': 'b', 'expires': run.now - 10})
    run.rt('probeResult', leaseId=lease, model='bad model', status=200)
    run.rt('mystery', leaseId=lease)
    run.rt('mystery', leaseId='nope')
    run.rt('sent', leaseId='nope')
    run.rt('finish', leaseId=lease)
    return run.snapshot()


def scenario_window_expiry_rotation():
    run = Run('window-expiry-rotation')
    run.configure({'windowSeconds': 120, 'initialRestSeconds': 30, 'maximumRestSeconds': 240})
    first = run.probe()
    lease, access = run.business(first)
    run.rt('plan', demand=True)
    run.advance(121)
    plan = run.rt('plan', demand=True)
    run.rt('plan', demand=False)
    second = run.probe()
    run.end_business(lease, access, delay=3)
    run.rt('plan', demand=True)
    run.rt('reserve', gateway=first['pair']['gateway'], generation=first['pair']['generation'], deadline=run.now + 5)
    run.advance(121)
    third = run.probe()
    run.advance(121)
    run.probe(verdict='29')
    run.advance(40)
    run.probe()
    run.read()
    return run.snapshot()


def scenario_cookie_expiry_rotation():
    run = Run('cookie-expiry-rotation')
    run.configure({'windowSeconds': 600, 'gateways': []})
    catalog = [{'gateway': 'unified-150', 'validForS': 100, 'country': 'CH'},
               {'gateway': 'unified-151', 'validForS': 2000, 'country': 'US'},
               {'gateway': 'unified-152', 'validForS': 3000}]
    first = run.probe(catalog=catalog, material={'state': 's', 'cookie': 'c', 'expires': run.now + 300})
    run.advance(100)
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 3000}])
    run.advance(150)
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 3000}])
    run.rt('plan', demand=False)
    run.rt('plan', demand=True, catalog=[{'gateway': 'evil', 'validForS': 1}])
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': -1}])
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': True}])
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 5, 'country': 'ch'}])
    run.rt('plan', demand=True, catalog='nope')
    second = run.probe()
    run.advance(400)
    run.configure({'mode': 'continuous'})
    run.rt('plan', demand=False)
    run.probe(demand=False)
    run.advance(700)
    run.rt('plan', demand=False, catalog=[])
    run.rt('plan', demand=True)
    return run.snapshot()


def scenario_round_yield():
    run = Run('full-round-yields')
    run.configure({'initialRestSeconds': 1, 'maximumRestSeconds': 8})
    run.register()
    turn = run.rt('plan', demand=True)['queueTurn']
    first = run.probe()
    lease = run.reserve(first)['leaseId']
    run.rt('invalidate', leaseId=lease)
    run.rt('finish', leaseId=lease)
    second = run.probe()
    run.advance(181)
    run.rt('plan', demand=True, alternatives=['B', 'C'])
    run.rt('claim', demand=True, queueTurn=turn, alternatives=['B', 'C'])
    run.rt('finish', leaseId=second['leaseId'])
    run.register()
    run.rt('plan', demand=True, ordinary=True)
    run.rt('plan', demand=True, alternatives=['B'], queueTurn=1.0)
    run.rt('plan', demand=True, alternatives=['A', 'bad id'])
    # 单账号 Key：不让位，重新开始新一轮。
    run2 = [run.probe(account='B'), run.probe(account='B')]
    run.advance(181)
    run.rt('plan', 'B', demand=True)
    run.rt('claim', 'B', demand=True)
    run.register()
    return run.snapshot()


def scenario_business_29_and_mismatch():
    run = Run('business-29-and-gateway-mismatch')
    first = run.probe()
    for returned, matches in ((False, True), (False, False), (True, True)):
        reserved = run.reserve(first)
        if '__error__' in reserved or not reserved.get('reserved'):
            first = run.probe()
            reserved = run.reserve(first)
        lease = reserved['leaseId']
        run.send(lease, 'business')
        run.rt('businessResult', leaseId=lease, status=200, stateReturned=returned, paired=True, gatewayMatches=matches, model='gpt-6-astra')
        run.rt('finish', leaseId=lease)
        run.advance(3700)
        first = run.probe()
    run.rt('businessResult', leaseId='missing', status=200)
    lease = run.reserve(first)['leaseId']
    run.rt('businessResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True)
    run.rt('probeResult', leaseId=lease, status=200)
    run.send(lease, 'business')
    run.rt('businessResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True)
    run.rt('finish', leaseId=lease)
    run.rt('reserve', gateway=first['pair']['gateway'], generation=first['pair']['generation'], deadline=run.now - 1)
    run.rt('reserve', gateway=first['pair']['gateway'], generation=first['pair']['generation'], deadline=run.now + 90000)
    run.rt('reserve', gateway=first['pair']['gateway'], generation=first['pair']['generation'], deadline='soon')
    run.rt('reserve', gateway=first['pair']['gateway'], generation=first['pair']['generation'], deadline=str(run.now + 50))
    run.rt('reserve', gateway=first['pair']['gateway'], generation='other', deadline=run.now + 50)
    run.rt('reserve', gateway=7, generation='other', deadline=run.now + 50)
    return run.snapshot()


def scenario_gateway_restriction_changes():
    run = Run('gateway-restriction-changes')
    run.register()
    first = run.probe()
    lease = run.reserve(first)['leaseId']
    run.configure({'gateways': ['unified-143']})
    run.rt('sent', leaseId=lease)
    run.rt('finish', leaseId=lease)
    claim = run.rt('claim', demand=True)
    run.rt('sent', leaseId=claim['leaseId'])
    run.configure({'gateways': ['unified-142']})
    run.rt('probeResult', leaseId=claim['leaseId'], status=200, stateReturned=False, paired=True, gatewayMatches=True,
           material={'state': 's', 'cookie': 'c', 'expires': run.now + 100})
    run.rt('finish', leaseId=claim['leaseId'])
    run.configure({'gateways': []})
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 600}])
    run.configure({'gateways': ['unified-199']})
    run.rt('plan', demand=True, catalog=[{'gateway': 'unified-150', 'validForS': 600}])
    run.rt('plan', demand=True)
    run.configure({'gateways': ['unified-142', 'unified-143'], 'groupIds': ['g1']})
    run.rt('plan', demand=True)
    run.configure({'poolUrl': 'https://pool.example.com'})
    run.read()
    return run.snapshot()


def scenario_dynamic_groups_and_recovery():
    run = Run('dynamic-groups-recovery')
    run.configure({'groupIds': ['grp_degraded'], 'dynamicGroups': True, 'normalGroupId': 'grp_normal',
                   'degradedGroupId': 'grp_degraded', 'recoverySeconds': 480, 'recoveryPauseSeconds': 1800})
    run.rt('regroup', direction='sideways')
    run.rt('regroup', direction='degraded', model='bad model!')
    event = run.rt('regroup', direction='degraded', model='gpt-6.1-sol')['move']
    run.rt('regroup', direction='degraded')
    run.rt('regroupResult', eventId=event['eventId'], status='moved')
    run.rt('regroupResult', eventId=event['eventId'], status='done')
    run.rt('regroupResult', eventId='nope', status='moved')
    run.advance(100)
    run.rt('regroup', direction='degraded')
    other = run.rt('regroup', direction='recovered')['move']
    run.rt('regroupResult', eventId=other['eventId'], status='failed')
    run.advance(700)
    run.rt('regroup', direction='degraded')
    # 恢复检测：成功、失败、不可用三条路径。
    for account, outcome in (('A', 'failed'), ('B', 'recovered'), ('C', 'unavailable')):
        claim = run.probe(account=account, material={'state': 's', 'cookie': 'c', 'expires': run.now + 3000})
        run.rt('recoveryCheck', account)
        run.advance(481)
        run.rt('recoveryCheck', account)
        run.rt('recoveryCheck', account)
        run.rt('recoveryResult', account, outcome=outcome)
        run.advance(299)
        run.rt('recoveryCheck', account)
        run.advance(2000)
        run.rt('recoveryCheck', account)
    run.rt('recoveryResult', outcome='maybe')
    # 带 29 的代次不能作为恢复证据；材料过期后同样不能。
    claim = run.probe(account='A', material={'state': 's', 'cookie': 'c', 'expires': run.now + 3000})
    lease = run.reserve(claim, 'A')['leaseId']
    run.rt('sent', leaseId=lease)
    run.rt('businessResult', leaseId=lease, status=200, stateReturned=True, paired=True, gatewayMatches=True)
    run.rt('finish', leaseId=lease)
    run.advance(1000)
    run.rt('recoveryCheck')
    run.advance(3000)
    claim = run.probe(account='B', material={'state': 's', 'cookie': 'c', 'expires': run.now + 700})
    run.advance(681)
    run.rt('recoveryCheck', 'B')
    run.configure({'dynamicGroups': False})
    run.rt('recoveryCheck', 'B')
    run.rt('regroup', direction='degraded')
    return run.snapshot()


def scenario_quota_and_queue():
    run = Run('quota-queue-disable-drag')
    run.configure({'initialRestSeconds': 1, 'maximumRestSeconds': 8})
    run.register()
    old = run.revision()
    run.rt('quota', exhausted=True)
    run.rt('quota', exhausted=True)
    run.reorder(['C', 'B', 'A'], revision=old)
    run.rt('quota', exhausted=False)
    run.register()
    run.rt('quota', exhausted='yes')
    run.rt('quota', 'bad id', exhausted=True)
    run.reorder(['B', 'C', 'A'])
    claim = run.probe('B')
    lease, access = run.business(claim, 'B')
    unsent = run.reserve(claim, 'B')['leaseId']
    run.toggle('B', False)
    run.rt('sent', 'B', leaseId=unsent)
    run.rt('reserve', 'B', gateway=claim['pair']['gateway'], generation=claim['pair']['generation'], deadline=run.now + 100)
    run.rt('claim', 'B', demand=True)
    run.rt('plan', 'B', demand=True)
    run.end_business(lease, access, 'B', delay=4)
    run.rt('finish', 'B', leaseId=unsent)
    run.register(exhausted=('C',))
    run.toggle('B', True)
    run.register()
    run.register(accounts=('A', 'A'))
    run.register(accounts=('A', 7))
    run.runtime({'action': 'active', 'candidates': 'x'})
    run.runtime({'action': 'active', 'candidates': [{'id': 'A'}]})
    run.admin('queue.update', {'revision': 'x', 'accountIds': ['A']})
    # 停用 / 额度耗尽时的 claim / plan 返回受阻原因。
    run.probe('A', verdict='29')
    run.probe('A', verdict='29')
    run.register(accounts=('A', 'B', 'C'), exhausted=('C',))
    run.rt('plan', demand=True)
    run.advance(5000)
    run.register()
    run.reorder(['A', 'C', 'B'])
    return run.snapshot()


def scenario_expiry_and_lease_reaping():
    run = Run('material-ticket-lease-expiry')
    first = run.probe(material={'state': 's1', 'cookie': 'c1', 'expires': run.now + 120})
    lease = run.reserve(first, ahead=50)['leaseId']
    run.rt('sent', leaseId=lease)
    run.rt('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, material={'state': 's2', 'cookie': 'c2', 'expires': run.now + 100})
    run.rt('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, material={'state': 's2', 'cookie': 'c2'})
    run.rt('ticketResult', leaseId=lease, model='gpt-6-astra', status=200, material={'state': 's2', 'cookie': 'c2', 'expires': run.now - 1})
    run.rt('ticketResult', leaseId=lease, model='GPT', status=200)
    run.advance(60)
    # 租约超时：被保守回收；已发送的从超时点静置。
    run.rt('plan', demand=True)
    run.rt('finish', leaseId=lease)
    run.rt('material', generation=first['pair']['generation'])
    run.advance(100)
    run.rt('material', generation=first['pair']['generation'])
    run.rt('ticket', generation=first['pair']['generation'], model='gpt-6-astra')
    # 探测租约在 60 秒内没有任何接触：回到 unknown；已接触的进入静置。
    claim = run.rt('claim', demand=True)
    run.advance(61)
    run.rt('plan', demand=True)
    claim = run.rt('claim', demand=True)
    run.rt('sent', leaseId=claim['leaseId'])
    run.advance(61)
    run.rt('plan', demand=True)
    run.rt('finish', leaseId=claim['leaseId'])
    run.advance(5000)
    run.rt('claim', demand=True)
    run.advance(61)
    run.rt('active')
    run.enable(False)
    run.rt('plan', demand=True)
    run.rt('config')
    run.rt('finish', leaseId='nope')
    run.enable(True)
    run.rt('config')
    run.restart()
    run.rt('plan', demand=True)
    return run.snapshot()


def scenario_disabled_plugin_in_flight():
    run = Run('disabled-plugin-inflight')
    first = run.probe()
    lease = run.reserve(first, ahead=500)['leaseId']
    run.send(lease, 'business')
    claim = run.rt('claim', 'B', demand=True)
    run.rt('sent', 'B', leaseId=claim['leaseId'])
    run.enable(False)
    run.advance(10)
    run.rt('finish', leaseId=lease)
    run.rt('finish', 'B', leaseId=claim['leaseId'])
    run.rt('claim', demand=True)
    run.rt('config')
    run.enable(True)
    run.rt('plan', 'B', demand=True)
    run.read()
    return run.snapshot()


def scenario_history_accesses():
    run = Run('history-accesses-and-timing')
    run.configure({'initialRestSeconds': 1, 'maximumRestSeconds': 8})
    first = run.probe()
    lease, access = run.business(first)
    run.advance(1)
    run.rt('businessResult', leaseId=lease, status=200, stateReturned=False, paired=True, gatewayMatches=True,
           access=run.access(access, end=False))
    lease2, b = run.business(first)
    run.end_business(lease, access, delay=1)
    run.end_business(lease2, b, delay=1)
    run.advance(7199)
    lease3, c = run.business(first)
    run.end_business(lease3, c, delay=1)
    run.advance(7300)
    run.stats()
    lease4, d = run.business(first)
    run.advance(8000)
    run.stats()
    run.end_business(lease4, d, delay=1)
    # 计时无效 / 超出范围 / 重复结束 / legacy 访问。
    lease5, e = run.business(first)
    bad = run.access(e)
    run.rt('finish', leaseId=lease5, access={**bad, 'elapsedMs': -1})
    lease6, f = run.business(first)
    run.rt('finish', leaseId=lease6, access={**run.access(f), 'elapsedMs': 'x'})
    lease7, g = run.business(first)
    run.rt('finish', leaseId=lease7, access={**run.access(g), 'startedAt': run.now + 1000})
    lease8, h = run.business(first, timing=False)
    run.advance(3)
    run.rt('finish', leaseId=lease8)
    lease9, i = run.business(first)
    ticket = run.send(lease9, 'ticket')
    run.advance(3)
    run.rt('ticketResult', leaseId=lease9, model='gpt-6-astra', status=200, access=run.access(ticket))
    run.end_business(lease9, i, delay=2)
    run.rt('finish', leaseId=lease9, access=run.access(i))
    lease10 = run.reserve(first)['leaseId']
    run.rt('sent', leaseId=lease10, timingVersion=2, accessId='y' * 81, purpose='business')
    run.rt('sent', leaseId=lease10, timingVersion=1, accessId='ok', purpose='business')
    run.rt('sent', leaseId=lease10, timingVersion=2, accessId='ok2', purpose=['x'])
    run.rt('sent', leaseId=lease10, timingVersion=2.0, accessId='ok3', purpose='nonsense')
    run.advance(20001)
    run.rt('active')
    run.advance(3)
    run.probe(verdict='29')
    run.advance(5)
    run.probe(verdict='29')
    run.advance(8)
    run.probe(verdict='21')
    return run.snapshot()


def scenario_malformed_requests():
    run = Run('malformed-requests')
    claim = run.probe()
    pair = claim['pair']
    ready = {'gateway': pair['gateway'], 'generation': pair['generation']}
    probing = run.rt('claim', 'B', demand=True)
    run.rt('sent', 'B', leaseId=probing['leaseId'])
    for message in [
        {'action': 'sent', 'accountId': 'A'},
        {'action': 'finish', 'accountId': 'A'},
        {'action': 'finish', 'leaseId': 'x'},
        {'accountId': 'A'},
        {'action': 'plan'},
        {'action': 'reserve', 'accountId': 'A', 'generation': 'g', 'deadline': 1},
        {'action': 'reserve', 'accountId': 'A', **ready, 'deadline': None},
        {'action': 'reserve', 'accountId': 'A', **ready, 'deadline': []},
        {'action': 'reserve', 'accountId': 'A', **ready, 'deadline': True},
        {'action': 'reserve', 'accountId': 'A', **ready},
        {'action': 'reserve', 'accountId': 'A', 'gateway': ready['gateway'], 'deadline': run.now + 10},
        {'action': 'material', 'accountId': 'A'},
        {'action': 'ticket', 'accountId': 'A'},
        {'action': 'ticket', 'accountId': 'A', 'generation': ready['generation']},
        {'action': 'ticket', 'accountId': 'A', 'generation': ready['generation'], 'model': 5},
        {'action': 'quota', 'accountId': 'A'},
        {'action': 'quota', 'accountId': 5, 'exhausted': True},
        {'action': 'plan', 'accountId': ''},
        {'action': 'plan', 'accountId': 'x' * 129},
        {'action': 7, 'accountId': 'A'},
        {'action': 7, 'accountId': 'A', 'leaseId': 'nope'},
        {'action': 'ticketResult', 'accountId': 'B', 'leaseId': probing['leaseId']},
        {'action': 'probeAttempt', 'accountId': 'B', 'leaseId': probing['leaseId'], 'phase': ['x'], 'retry': 0},
        {'action': 'probeAttempt', 'accountId': 'B', 'leaseId': probing['leaseId'], 'phase': 'replay', 'retry': None},
        {'action': 'probeAttempt', 'accountId': 'B', 'leaseId': probing['leaseId'], 'phase': 'replay', 'retry': False, 'status': 'x'},
        {'action': 'probeResult', 'accountId': 'B', 'leaseId': probing['leaseId'], 'status': 200.0, 'paired': True, 'gatewayMatches': True,
         'material': 'plain'},
        {'action': 'regroup', 'accountId': 'A', 'direction': ['degraded']},
        {'action': 'regroup', 'accountId': 'A'},
        {'action': 'regroupResult', 'accountId': 'A', 'eventId': 5, 'status': 'moved'},
        {'action': 'recoveryResult', 'accountId': 'A', 'outcome': ['failed']},
        {'action': 'recoveryResult', 'accountId': 'A'},
        {'action': 'active', 'candidates': [{'id': 5, 'exhausted': False}]},
        {'action': 'active', 'candidates': [5]},
        {'action': 'claim', 'accountId': 'A', 'alternatives': 'AB'},
        {'action': 'claim', 'accountId': 'A', 'alternatives': 5},
        {'action': 'claim', 'accountId': 'A', 'alternatives': 0},
        {'action': 'plan', 'accountId': 'A', 'catalog': [5]},
        {'action': 'plan', 'accountId': 'A', 'catalog': [{'gateway': 5, 'validForS': 5}]},
        {'action': 'plan', 'accountId': 'A', 'catalog': {'gateway': 'unified-1'}},
        {'action': 'plan', 'accountId': 'A', 'catalog': None, 'demand': 'yes', 'ordinary': 1},
        {'action': 'sent', 'accountId': 'B', 'leaseId': probing['leaseId'], 'timingVersion': 2, 'accessId': 5, 'purpose': 'replay'},
        {'action': 'finish', 'accountId': 'B', 'leaseId': probing['leaseId'], 'access': 'bad'},
        {'action': 'finish', 'accountId': 'B', 'leaseId': probing['leaseId'], 'access': {'id': 'x'}},
        {'action': 'finish', 'accountId': 'A', 'leaseId': 5},
    ]:
        run.runtime(message)
    return run.snapshot()


def scenario_independent_accounts_and_restart():
    run = Run('independent-accounts-restart')
    run.register()
    a = run.probe('A')
    b = run.probe('B')
    run.read(accountId='B')
    run.restart()
    run.rt('active', candidates=[{'id': 'A', 'exhausted': False}, {'id': 'B', 'exhausted': False}])
    run.advance(200)
    run.rt('plan', 'A', demand=True)
    run.rt('plan', 'B', demand=True)
    run.probe('C', verdict='29')
    run.probe('C', verdict='29')
    run.advance(4000)
    run.probe('C')
    run.rt('active')
    for account in ('A', 'B', 'C'):
        run.rt('claim', account, demand=True)
    return run.snapshot()


# ---------------------------------------------------------------- 随机序列

GATEWAYS = ['unified-142', 'unified-143', 'unified-150']


class Fuzz(Run):
    def __init__(self, seed, steps):
        super().__init__('fuzz-%d' % seed)
        self.random = random.Random(seed)
        self.target = steps
        self.leases = []      # 未结束的租约：{id, account, kind, generation, gateway}
        self.generations = []  # 已见过的代次：{account, gateway, generation}
        self.accesses = {}

    def pick(self, items):
        return self.random.choice(items)

    def lease(self):
        return self.pick(self.leases) if self.leases and self.random.random() < 0.9 else None

    def material(self, valid=True):
        if valid and self.random.random() < 0.9:
            return {'state': 's%d' % self.random.randrange(1000), 'cookie': 'c%d' % self.random.randrange(1000),
                    'expires': self.now + self.pick([30, 61, 90, 600, 5000, 100000])}
        return self.pick([{'state': 'x'}, {'state': 'a', 'cookie': 'b', 'expires': self.now - 5}, 'text',
                          {'state': 'a\r', 'cookie': 'b', 'expires': self.now + 10}])

    def account(self):
        return self.pick(['A', 'A', 'A', 'B', 'B', 'C'])

    def catalog(self):
        r = self.random.random()
        if r < 0.55:
            return {}
        if r < 0.62:
            return {'catalog': self.pick([[], 'bad', [{'gateway': 'evil', 'validForS': 1}]])}
        return {'catalog': [{'gateway': g, 'validForS': self.pick([30, 100, 500, 3000]), 'country': self.pick([None, 'CH', 'US'])}
                            for g in self.random.sample(GATEWAYS, self.random.randint(1, 3))]}

    def access_for(self, lease_id, end):
        known = self.accesses.get(lease_id)
        if not known or self.random.random() < 0.15:
            return {}
        item = self.pick(known)
        elapsed = (self.now - item['startedAt']) * 1000 * self.pick([1, 1, 1, 0.5, 0])
        if self.random.random() < 0.05:
            elapsed = self.pick([-1, 'x', 1e12])
        return {'access': {'id': item['id'], 'startedAt': item['startedAt'], 'elapsedMs': elapsed, 'ended': end,
                           'outcome': self.pick(['completed', 'failed', 'headers', 'weird', 'cancelled'])}}

    def step_claim(self):
        account = self.account()
        data = {'demand': self.pick([True, True, False])}
        data.update(self.catalog())
        if self.random.random() < 0.3:
            data['queueTurn'] = self.pick([1, 2, 3, 1.0, 'x'])
            data['alternatives'] = self.pick([['B'], ['A', 'C'], [], ['B', 'C']])
        if self.random.random() < 0.1:
            data['ordinary'] = True
        action = self.pick(['claim', 'claim', 'plan'])
        result = self.rt(action, account, **data)
        if isinstance(result, dict) and result.get('claimed'):
            self.leases.append({'id': result['leaseId'], 'account': account, 'kind': 'probe',
                                'generation': result['pair']['generation'], 'gateway': result['pair']['gateway']})
            self.generations.append({'account': account, 'gateway': result['pair']['gateway'], 'generation': result['pair']['generation']})

    def step_flow(self):
        """一次相对完整的流程（带随机的跳步与异常），让随机序列能走到发布窗口、退役、静置等深处状态。"""
        account = self.account()
        kind = self.pick(['probe', 'probe', 'business', 'business', 'ticket'])
        if kind == 'probe':
            data = {'demand': self.pick([True, True, False])}
            data.update(self.catalog())
            claim = self.rt('claim', account, **data)
            if not (isinstance(claim, dict) and claim.get('claimed')):
                return
            lease = claim['leaseId']
            self.generations.append({'account': account, 'gateway': claim['pair']['gateway'], 'generation': claim['pair']['generation']})
            if self.random.random() < 0.9:
                access = self.send(lease, 'bootstrap', account)
                self.advance(self.pick([0.1, 0.5, 2]))
                if self.random.random() < 0.6:
                    self.rt('probeAttempt', account, leaseId=lease, access=self.access(access), phase='bootstrap', retry=0, status=200,
                            stateReturned=True)
                    access = self.send(lease, 'replay', account)
                    self.advance(self.pick([0.1, 0.5]))
                    self.rt('probeAttempt', account, leaseId=lease, access=self.access(access), phase='replay',
                            retry=self.pick([0, 1]), status=self.pick([200, 200, 402]), stateReturned=False)
            verdict = self.pick(['21', '21', '21', '29', 'fail', 'mismatch', 'status'])
            data = dict(leaseId=lease, status=200, stateReturned=verdict == '29', paired=True, gatewayMatches=verdict != 'mismatch',
                        durationMs=self.pick([None, 50, 900]), preparationStartedAt=self.now - 1)
            if verdict == 'fail':
                data['failed'] = True
            if verdict == 'status':
                data['status'] = self.pick([401, 429, 500])
            if verdict == '21' or self.random.random() < 0.2:
                data['material'] = self.material()
            if self.random.random() < 0.9:
                self.rt('probeResult', account, **data)
            if self.random.random() < 0.85:
                self.advance(self.pick([0, 0.2, 1]))
                self.rt('finish', account, leaseId=lease)
            else:
                self.leases.append({'id': lease, 'account': account, 'kind': 'probe', 'generation': claim['pair']['generation'],
                                    'gateway': claim['pair']['gateway']})
            return
        if not self.generations:
            return
        known = self.pick([g for g in self.generations if g['account'] == account] or self.generations)
        account = known['account']
        reserved = self.rt('reserve', account, gateway=known['gateway'], generation=known['generation'],
                           deadline=self.now + self.pick([30, 300, 20000]))
        if not (isinstance(reserved, dict) and reserved.get('reserved')):
            return
        lease = reserved['leaseId']
        access = None
        if self.random.random() < 0.9:
            access = self.send(lease, 'business' if kind == 'business' else 'ticket', account)
            self.advance(self.pick([0.2, 1, 5]))
        if kind == 'ticket' and access:
            self.rt('ticketResult', account, leaseId=lease, model='gpt-6-astra', status=200, stateReturned=self.random.random() < 0.5,
                    access=self.access(access), material=self.material())
            access = self.send(lease, 'business', account)
            self.advance(self.pick([0.2, 1]))
        if self.random.random() < 0.8:
            self.rt('businessResult', account, leaseId=lease, status=200, stateReturned=self.random.random() < 0.3, paired=True,
                    gatewayMatches=self.random.random() < 0.9, access=self.access(access, end=False) if access else None,
                    model=self.pick([None, 'gpt-6-astra']))
        if self.random.random() < 0.85:
            self.advance(self.pick([0, 1, 3]))
            self.rt('finish', account, leaseId=lease, access=self.access(access) if access and self.random.random() < 0.9 else None)
        else:
            self.leases.append({'id': lease, 'account': account, 'kind': 'business', 'generation': known['generation'], 'gateway': known['gateway']})

    def step_reserve(self):
        if not self.generations:
            return
        known = self.pick(self.generations)
        data = {'gateway': known['gateway'], 'generation': known['generation'],
                'deadline': self.now + self.pick([20, 100, 5000, 100000, -5])}
        if self.random.random() < 0.05:
            data['generation'] = 'unknown-generation'
        result = self.rt('reserve', known['account'], **data)
        if isinstance(result, dict) and result.get('reserved'):
            self.leases.append({'id': result['leaseId'], 'account': known['account'], 'kind': 'business',
                                'generation': known['generation'], 'gateway': known['gateway']})

    def step_lease(self):
        lease = self.lease()
        if lease is None:
            self.rt(self.pick(['sent', 'finish', 'probeResult', 'businessResult']), self.account(), leaseId='f' * 32)
            return
        account, lease_id = lease['account'], lease['id']
        action = self.pick(['sent', 'sent', 'probeAttempt', 'probeResult', 'probeResult', 'businessResult', 'ticketResult',
                            'invalidate', 'finish', 'finish'])
        if action == 'sent':
            data = {}
            if self.random.random() < 0.7:
                access_id = 'a%d' % self.random.randrange(10**6)
                data = {'timingVersion': 2, 'accessId': access_id, 'purpose': self.pick(['business', 'ticket', 'bootstrap', 'replay'])}
                self.accesses.setdefault(lease_id, []).append({'id': access_id, 'startedAt': self.now})
            self.rt('sent', account, leaseId=lease_id, **data)
        elif action == 'probeAttempt':
            data = dict(phase=self.pick(['bootstrap', 'replay', 'replay']), retry=self.pick([0, 0, 1, 1.0, 2]),
                        status=self.pick([200, 200, 402, 429]), stateReturned=self.random.random() < 0.3,
                        durationMs=self.pick([None, 5, 42]))
            data.update(self.access_for(lease_id, True))
            self.rt('probeAttempt', account, leaseId=lease_id, **data)
        elif action in ('probeResult', 'businessResult'):
            if self.random.random() < 0.9:
                action = 'probeResult' if lease['kind'] == 'probe' else 'businessResult'
            data = dict(status=self.pick([200, 200, 200, 200, 401, 429, '200']), stateReturned=self.random.random() < 0.35,
                        paired=self.random.random() < 0.95, gatewayMatches=self.random.random() < 0.93,
                        failed=self.random.random() < 0.08, durationMs=self.pick([None, 5, 300, 70000, -1]))
            if self.random.random() < 0.5:
                data['model'] = self.pick(['gpt-6-astra', 'gpt-6.1-sol', 'BAD', None])
            if action == 'probeResult':
                data['preparationStartedAt'] = self.pick([self.now - 1, self.now - 100, lease.get('start', self.now)])
                if self.random.random() < 0.8:
                    data['material'] = self.material()
            data.update(self.access_for(lease_id, self.random.random() < 0.7))
            self.rt(action, account, leaseId=lease_id, **data)
        elif action == 'ticketResult':
            data = dict(model=self.pick(['gpt-6-astra', 'gpt-6.1-sol', 'gpt-x']), status=200,
                        stateReturned=self.random.random() < 0.5, durationMs=5)
            if self.random.random() < 0.7:
                data['material'] = self.material()
            data.update(self.access_for(lease_id, True))
            self.rt('ticketResult', account, leaseId=lease_id, **data)
        elif action == 'invalidate':
            self.rt('invalidate', account, leaseId=lease_id)
        elif action == 'finish':
            data = self.access_for(lease_id, True)
            result = self.rt('finish', account, leaseId=lease_id, **data)
            if isinstance(result, dict) and result.get('accepted') and self.random.random() < 0.95:
                self.leases = [item for item in self.leases if item['id'] != lease_id]

    def step_config(self):
        choice = self.random.random()
        if choice < 0.3:
            self.configure(self.pick([{'gateways': []}, {'gateways': ['unified-142']}, {'gateways': ['unified-143']},
                                      {'gateways': ['unified-142', 'unified-143']}, {'gateways': ['unified-150']}]))
        elif choice < 0.45:
            self.configure({'mode': self.pick(['lazy', 'continuous'])})
        elif choice < 0.6:
            self.configure({'windowSeconds': self.pick([30, 60, 180, 600]), 'initialRestSeconds': self.pick([1, 30, 600, 3600]),
                            'maximumRestSeconds': self.pick([60, 480, 86400])})
        elif choice < 0.75:
            self.configure({'dynamicGroups': True, 'groupIds': ['g_deg', 'g_norm'], 'normalGroupId': 'g_norm', 'degradedGroupId': 'g_deg',
                            'recoverySeconds': self.pick([60, 480]), 'recoveryPauseSeconds': self.pick([100, 3600])})
        elif choice < 0.8:
            self.configure({'dynamicGroups': False})
        elif choice < 0.9:
            self.configure(self.pick([{'poolUrl': 'http://bad'}, {'probeIntervalMs': 7.5}, {'windowSeconds': 99999}, {'zzz': 1}]))
        else:
            self.configure({'groupIds': self.pick([[], ['g_deg'], ['g_deg', 'g_norm']])})

    def step_queue(self):
        choice = self.random.random()
        if choice < 0.35:
            accounts = self.random.sample(['A', 'B', 'C'], self.random.randint(2, 3))
            self.reorder(accounts)
        elif choice < 0.5:
            self.reorder(['C', 'B', 'A'], revision=self.pick([0, 1, 99]))
        elif choice < 0.8:
            self.toggle(self.account(), self.random.random() < 0.6)
        else:
            self.register(accounts=self.pick([('A', 'B', 'C'), ('A', 'B'), ('B',), ('C', 'A')]),
                          exhausted=self.pick([(), (), ('C',), ('A',), ('B', 'C')]))

    def step_groups(self):
        account = self.account()
        choice = self.random.random()
        if choice < 0.3:
            self.rt('regroup', account, direction=self.pick(['degraded', 'recovered', 'sideways']), model=self.pick(['gpt-6.1-sol', None, 'BAD!']))
        elif choice < 0.5 and self.steps:
            events = [s['expect']['ok']['move']['eventId'] for s in self.steps
                      if s['op'] == 'runtime' and 'ok' in s['expect'] and isinstance(s['expect']['ok'], dict)
                      and isinstance(s['expect']['ok'].get('move'), dict)]
            if events:
                self.rt('regroupResult', account, eventId=self.pick(events + ['0' * 32]), status=self.pick(['moved', 'failed', 'unchanged', 'unsupported', 'bad']))
        elif choice < 0.8:
            self.rt('recoveryCheck', account)
        else:
            self.rt('recoveryResult', account, outcome=self.pick(['recovered', 'failed', 'unavailable', 'x']))

    def step_other(self):
        choice = self.random.random()
        if choice < 0.2:
            self.rt('quota', self.account(), exhausted=self.pick([True, False, False]))
        elif choice < 0.35 and self.generations:
            known = self.pick(self.generations)
            self.rt(self.pick(['material', 'ticket']), known['account'], generation=known['generation'], model=self.pick(['gpt-6-astra', 'gpt-6.1-sol']))
        elif choice < 0.5:
            self.rt('active')
        elif choice < 0.6:
            self.register(accounts=self.pick([('A', 'B', 'C'), ('A',), ('B', 'C')]))
        elif choice < 0.7:
            self.rt('config', self.account())
        elif choice < 0.78:
            self.read(**self.pick([{}, {'accountId': 'A'}, {'gateway': 'unified-142', 'limit': 5}, {'before': 20}]))
        elif choice < 0.86:
            self.stats(**self.pick([{}, {'view': 'summary'}, {'view': 'cooling'}, {'view': 'records', 'pageSize': 3}]))
        elif choice < 0.92:
            self.restart()
        else:
            self.enable(False)
            for _ in range(self.random.randint(1, 3)):
                self.rt(self.pick(['plan', 'claim', 'config', 'finish', 'sent']), self.account(), leaseId=(self.lease() or {'id': 'x'})['id'], demand=True)
                self.advance(self.pick([1, 10]))
            self.enable(True)

    def run(self):
        self.register()
        while len(self.steps) < self.target:
            self.advance(self.pick([0, 0, 0.1, 1, 5, 30, 61, 181, 600, 3601, 7201]))
            roll = self.random.random()
            if roll < 0.30:
                self.step_flow()
            elif roll < 0.38:
                self.step_claim()
            elif roll < 0.43:
                self.step_reserve()
            elif roll < 0.63:
                self.step_lease()
            elif roll < 0.71:
                self.step_config()
            elif roll < 0.77:
                self.step_queue()
            elif roll < 0.84:
                self.step_groups()
            else:
                self.step_other()
        return self.snapshot()


SCENARIOS = [
    scenario_lazy_ticket_success, scenario_ticket_29_backoff, scenario_ticket_failures, scenario_window_expiry_rotation,
    scenario_cookie_expiry_rotation, scenario_round_yield, scenario_business_29_and_mismatch,
    scenario_gateway_restriction_changes, scenario_dynamic_groups_and_recovery, scenario_quota_and_queue,
    scenario_expiry_and_lease_reaping, scenario_disabled_plugin_in_flight, scenario_history_accesses,
    scenario_independent_accounts_and_restart, scenario_malformed_requests,
]
FUZZ_SEEDS = list(range(101, 125))
FUZZ_STEPS = 600


def generate(out):
    out.mkdir(parents=True, exist_ok=True)
    sequences = []
    for build in SCENARIOS:
        sequences.append(build())
    for seed in FUZZ_SEEDS:
        sequences.append(Fuzz(seed, FUZZ_STEPS).run())
    (out / 'runtime-cases.json').write_text(json.dumps({'sequences': sequences}, ensure_ascii=False, allow_nan=False), encoding='utf-8')
    total = sum(len(item['steps']) for item in sequences)
    errors = sum(1 for item in sequences for step in item['steps'] if any(k in step['expect'] for k in ('error', 'forbidden', 'invalid')))
    print('python sequences: %d, steps: %d (of which error expectations: %d)' % (len(sequences), total, errors))
    for item in sequences:
        failing = sum(1 for step in item['steps'] if any(k in step['expect'] for k in ('error', 'forbidden', 'invalid')))
        print('  %-44s %4d steps %3d errors' % (item['name'], len(item['steps']), failing))


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False)


def verify_rust(out):
    """Python 读取 Rust 写出的数据库（运行序列的最终状态），页面读取与统计必须与 Python 自己的运行结果一致。"""
    document = json.loads((out / 'runtime-cases.json').read_text(encoding='utf-8'))
    written = out / 'rust-written'
    report, mismatches = [], 0
    for sequence in document['sequences']:
        source = written / (sequence['name'] + '.sqlite3')
        if not source.exists():
            report.append({'sequence': sequence['name'], 'compared': 0, 'match': False, 'note': 'missing database'})
            mismatches += 1
            continue
        root = Path(tempfile.mkdtemp(prefix='gp-rust-runtime-'))
        try:
            target = root / 'gateway-pool-runtime'
            target.mkdir(parents=True)
            shutil.copyfile(source, target / 'state.sqlite3')
            clock = [sequence['finalClock']]
            store = base.Store(root)
            pool = GatewayPool(root, store, lambda: clock[0])
            # 取序列末尾的读取步骤（snapshot 里记录的 read / history.read）。
            reads = [s for s in sequence['steps'] if s['op'] == 'admin' and s['method'] in ('read', 'history.read')][-11:]
            compared, bad = 0, 0
            # 读取步骤在原序列里是在 finalClock 之前的同一时刻发生的；逐个用记录的时钟重放。
            for step in reads:
                clock[0] = step['clock']
                actual = clean(py_admin(pool, step['method'], step['data']))
                compared += 1
                if canonical(actual) != canonical(step['expect']):
                    bad += 1
            mismatches += bad
            report.append({'sequence': sequence['name'], 'compared': compared, 'mismatches': bad, 'match': bad == 0})
        finally:
            shutil.rmtree(root, ignore_errors=True)
    (out / 'rust-to-python-runtime-report.json').write_text(json.dumps({'sequences': report, 'mismatches': mismatches}, ensure_ascii=False, indent=1), encoding='utf-8')
    print('rust-to-python: %d sequences, %d reads mismatching' % (len(report), mismatches))
    return 0 if mismatches == 0 else 1


if __name__ == '__main__':
    command = sys.argv[1]
    out = Path(sys.argv[3]).resolve()
    if command == 'generate':
        generate(out)
    elif command == 'verify-rust':
        sys.exit(verify_rust(out))
    else:
        raise SystemExit('unknown command')

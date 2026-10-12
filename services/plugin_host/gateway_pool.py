"""持久化网关池事实与统计。SQLite 事务同时保护线程及 A/B 网关进程。"""
from contextlib import contextmanager
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import time
from urllib.parse import urlsplit
import uuid


class GatewayPool:
    def __init__(self, root, store, clock=time.time):
        self.root = Path(root) / 'gateway-pool-runtime'
        self.root.mkdir(parents=True, exist_ok=True)
        self.root.chmod(0o750)
        self.path = self.root / 'state.sqlite3'
        self.store, self.clock = store, clock
        self.cached_policy = None
        with self.db() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS pairs (account TEXT NOT NULL, gateway TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,gateway));
                CREATE TABLE IF NOT EXISTS leases (id TEXT PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, generation TEXT NOT NULL, kind TEXT NOT NULL, expires REAL NOT NULL, sent INTEGER NOT NULL DEFAULT 0, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS observations (id INTEGER PRIMARY KEY, at REAL NOT NULL, account TEXT NOT NULL, gateway TEXT NOT NULL, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS switches (id INTEGER PRIMARY KEY, at REAL NOT NULL, account TEXT NOT NULL, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS materials (account TEXT NOT NULL, generation TEXT NOT NULL, expires REAL NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,generation));
                CREATE TABLE IF NOT EXISTS model_tickets (account TEXT NOT NULL, generation TEXT NOT NULL, model TEXT NOT NULL, expires REAL NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account,generation,model));
                CREATE INDEX IF NOT EXISTS observations_pair ON observations(account,gateway,at);
            ''')
        self.path.chmod(0o640)

    @contextmanager
    def db(self):
        db = sqlite3.connect(self.path, timeout=5, isolation_level=None)
        db.row_factory = sqlite3.Row
        try:
            db.execute('BEGIN IMMEDIATE')
            yield db
            db.commit()
        except BaseException:
            db.rollback()
            raise
        finally:
            db.close()

    def policy(self):
        state = self.store.state('gateway-pool')
        if not state.get('enabled') or not state.get('current'):
            raise PermissionError('网关池插件未启用')
        version = state['current']
        if self.cached_policy and self.cached_policy[0] == version:
            return self.cached_policy[1]
        manifest = self.store.manifest('gateway-pool', version)
        entry = 'worker/policy.py'
        path = self.store.directory('gateway-pool') / 'releases' / version / entry
        if manifest.get('policyWorker') != entry or path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != manifest['resources'][entry]:
            raise ValueError('网关池策略校验失败')
        spec = importlib.util.spec_from_file_location('gateway_pool_policy_' + version, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.cached_policy = version, module
        return module

    def settings(self, db, *, private=False):
        policy = self.policy()
        row = db.execute('SELECT data FROM settings WHERE id=1').fetchone()
        result = {**policy.DEFAULTS, **(json.loads(row['data']) if row else {})}
        if not private:
            result['poolTokenConfigured'] = bool(result.pop('poolToken', None))
        return result

    def configure(self, data):
        policy = self.policy()
        allowed = set(policy.DEFAULTS) | {'poolToken'}
        if set(data) - allowed:
            raise ValueError('网关池设置字段无效')
        with self.db() as db:
            settings = {**self.settings(db, private=True), **data}
            for name, maximum in [('windowSeconds', 3600), ('initialRestSeconds', 86400), ('maximumRestSeconds', 86400), ('probeIntervalMs', 1000)]:
                settings[name] = policy.finite_seconds(settings[name], name, maximum)
            if not settings['probeIntervalMs'].is_integer():
                raise ValueError('候选间隔必须是整数毫秒')
            settings['probeIntervalMs'] = int(settings['probeIntervalMs'])
            if settings['initialRestSeconds'] > settings['maximumRestSeconds'] or settings['mode'] not in {'lazy', 'continuous'}:
                raise ValueError('静置范围或模式无效')
            for field, pattern in [('groupIds', r'[A-Za-z0-9_-]{1,128}'), ('gateways', r'unified-[0-9]{1,6}')]:
                values = settings[field]
                if not isinstance(values, list) or len(values) > 512 or any(not isinstance(v, str) or not re.fullmatch(pattern, v) for v in values) or len(set(values)) != len(values):
                    raise ValueError(field + '无效')
            # 打票模型只接受官方模型标识格式；默认 6.1 Sol。
            if not isinstance(settings['probeModel'], str) or not re.fullmatch(r'gpt-[0-9a-z.-]{1,60}', settings['probeModel']):
                raise ValueError('打票模型无效')
            mapping = settings['poolAccountIds']
            if not isinstance(mapping, dict) or len(mapping) > 512 or any(not isinstance(k, str) or not isinstance(v, str) or not re.fullmatch(r'[A-Za-z0-9:_-]{1,128}', k) or not re.fullmatch(r'[A-Za-z0-9:_.\/-]{1,160}', v) for k, v in mapping.items()):
                raise ValueError('池账号映射无效')
            url = urlsplit(settings['poolUrl'])
            if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment or url.path not in {'', '/'}:
                raise ValueError('池服务地址必须是 HTTPS 根地址')
            token = settings.get('poolToken', '')
            if not isinstance(token, str) or len(token) > 1024 or any(ord(c) < 33 or ord(c) > 126 for c in token):
                raise ValueError('池服务凭证无效')
            # 已删除的候选立即退役，已发送的租约仍可排空；未发送的旧预约会被 sent 拒绝。
            for row in db.execute('SELECT data FROM pairs').fetchall():
                pair = json.loads(row['data'])
                if pair['gateway'] not in settings['gateways'] and pair['state'] in {'ready','preparing'}:
                    policy.retire(pair, settings, degraded=False)
                    policy.settle(pair)
                    self.save(db,pair)
            db.execute('INSERT OR REPLACE INTO settings VALUES (1,?)', (json.dumps(settings),))
            return self.settings(db)

    def settings_for_ui(self):
        with self.db() as db:
            return self.settings(db)

    @staticmethod
    def pair(db, account, gateway):
        row = db.execute('SELECT data FROM pairs WHERE account=? AND gateway=?', (account, gateway)).fetchone()
        return json.loads(row['data']) if row else {'accountId': account, 'gateway': gateway, 'state': 'unknown', 'inFlight': 0}

    @staticmethod
    def save(db, pair):
        db.execute('INSERT OR REPLACE INTO pairs VALUES (?,?,?)', (pair['accountId'], pair['gateway'], json.dumps(pair)))

    def reap(self, db, settings, policy, now):
        # 请求租约超时后保守地从超时点重新静置；重启不能把未完成访问当成已休息。
        for lease in db.execute('SELECT * FROM leases WHERE expires<=?', (now,)).fetchall():
            pair = self.pair(db, lease['account'], lease['gateway'])
            pair['inFlight'] = max(0, pair.get('inFlight', 0) - 1)
            if lease['sent']:
                pair['lastContactAt'] = max(pair.get('lastContactAt', 0), lease['expires'])
            if lease['kind'] == 'probe' and pair.get('generation') == lease['generation']:
                pair.update(state='draining', restSeconds=policy.initial(settings, pair.get('learnedRestSeconds')), retiredReason='probe_interrupted')
            policy.settle(pair)
            self.save(db, pair)
            db.execute('DELETE FROM leases WHERE id=?', (lease['id'],))

    def runtime(self, data):
        # 停用/卸载后仍允许旧在途请求归还租约，不能依赖已卸载的策略代码。
        if data.get('action') == 'finish':
            with self.db() as db:
                lease = db.execute('SELECT * FROM leases WHERE id=? AND account=?', (data['leaseId'], data['accountId'])).fetchone()
                if not lease:
                    return {'accepted': False}
                pair = self.pair(db, lease['account'], lease['gateway'])
                pair['inFlight'] = max(0, pair.get('inFlight', 0)-1)
                if lease['sent']:
                    pair['lastContactAt'] = max(pair.get('lastContactAt', 0), self.clock())
                if lease['kind'] == 'probe' and pair['state'] == 'preparing' and pair.get('generation') == lease['generation']:
                    # 停用或结果上报中断时，仍完成探测退役，不留下永久 preparing。
                    row = db.execute('SELECT data FROM settings WHERE id=1').fetchone()
                    config = json.loads(row['data']) if row else {}
                    waited = pair.get('probeRestSeconds')
                    rest = waited*2 if waited is not None else pair.get('learnedRestSeconds') or config.get('initialRestSeconds',3600)
                    pair.update(state='draining', restSeconds=min(config.get('maximumRestSeconds',86400),rest), retiredReason='probe_interrupted')
                if pair['state'] == 'draining' and not pair['inFlight']:
                    if pair.get('lastContactAt') is None:
                        pair.update(state='unknown', restUntil=None)
                    else:
                        pair.update(state='cooling', restUntil=pair['lastContactAt']+pair['restSeconds'])
                self.save(db, pair)
                db.execute('DELETE FROM leases WHERE id=?', (lease['id'],))
                return {'accepted': True}
        if data.get('action') == 'config' and not self.store.state('gateway-pool').get('enabled'):
            return {'enabled': False}
        policy = self.policy()
        now = self.clock()
        action = data['action']
        with self.db() as db:
            settings = self.settings(db, private=True)
            self.reap(db, settings, policy, now)
            db.execute('DELETE FROM materials WHERE expires<=?', (now,))
            # 其他模型的票只属于当前可用窗口；组合退役或换代后立即作废。
            db.execute("DELETE FROM model_tickets WHERE expires<=? OR NOT EXISTS (SELECT 1 FROM pairs p WHERE p.account=model_tickets.account AND json_extract(p.data,'$.generation')=model_tickets.generation AND json_extract(p.data,'$.state')='ready')", (now,))
            if action == 'config':
                return {**settings, 'enabled': True}
            account = data['accountId']
            if not isinstance(account, str) or not re.fullmatch(r'[A-Za-z0-9:_-]{1,128}', account):
                raise ValueError('账号标识无效')
            if action == 'material':
                row = db.execute('SELECT data FROM materials WHERE account=? AND generation=? AND expires>?', (account, data['generation'], now)).fetchone()
                return json.loads(row['data']) if row else None
            if action == 'ticket':
                row = db.execute('SELECT data FROM model_tickets WHERE account=? AND generation=? AND model=? AND expires>?', (account, data['generation'], self.model(data), now)).fetchone()
                return json.loads(row['data']) if row else None
            if action in {'plan', 'claim'}:
                candidates = settings['gateways']
                pairs = [self.pair(db, account, gateway) for gateway in candidates]
                ready = next((p for p in pairs if p['state'] == 'ready'), None)
                due = ready and policy.rotation_due(ready, settings, now, demand=data.get('demand') is True)
                candidates = [p for p in pairs if p['state'] == 'unknown' or p['state'] == 'cooling' and p.get('restUntil', 0) <= now]
                candidates.sort(key=lambda p: (p.get('lastContactAt', 0), p['gateway']))
                probe = candidates[0] if candidates and (data.get('demand') is True or settings['mode'] == 'continuous') and (not ready or due) else None
                if action == 'plan':
                    return {'ready': ready, 'probe': probe, 'rotationDue': bool(due), 'preparing': any(p['state'] == 'preparing' for p in pairs)}
                if not probe or any(p['state'] == 'preparing' for p in pairs):
                    return {'claimed': False}
                generation = uuid.uuid4().hex
                probe.update(generation=generation, state='preparing', probeRestSeconds=probe.get('restSeconds') if probe.get('lastContactAt') is not None else None,
                             observedIdleSeconds=max(0, now-probe['lastContactAt']) if probe.get('lastContactAt') is not None else None,
                             preparationStartedAt=now, firstContactAt=None, inFlight=probe['inFlight']+1)
                self.save(db, probe)
                db.execute('INSERT INTO leases VALUES (?,?,?,?,?,?,0,?)', (generation, account, probe['gateway'], generation, 'probe', now+60, '{}'))
                return {'claimed': True, 'leaseId': generation, 'pair': probe}
            if action == 'reserve':
                pair = self.pair(db, account, data['gateway'])
                if pair['state'] != 'ready' or pair.get('generation') != data['generation']:
                    return {'reserved': False}
                lease_id = uuid.uuid4().hex
                expires = float(data['deadline'])
                if not now < expires <= now+86400:
                    raise ValueError('请求截止时间无效')
                pair['inFlight'] += 1
                self.save(db, pair)
                db.execute('INSERT INTO leases VALUES (?,?,?,?,?,?,0,?)', (lease_id, account, pair['gateway'], pair['generation'], 'business', expires, '{}'))
                return {'reserved': True, 'leaseId': lease_id}
            lease = db.execute('SELECT * FROM leases WHERE id=? AND account=?', (data['leaseId'], account)).fetchone()
            if not lease:
                return {'accepted': False}
            pair = self.pair(db, account, lease['gateway'])
            if action == 'sent':
                # 保留尚未发送的请求：已切换时重新选择，不能凭旧预约发送到退役组合。
                if lease['kind'] == 'business' and (pair['state'] != 'ready' or pair.get('generation') != lease['generation']):
                    return {'accepted': False}
                if not lease['sent'] and lease['kind'] == 'probe':
                    previous = pair.get('lastContactAt')
                    pair.update(firstContactAt=now, previousContactAt=previous,
                                observedIdleSeconds=max(0, now-previous) if previous is not None else None)
                db.execute('UPDATE leases SET sent=1 WHERE id=?', (lease['id'],))
                pair['lastContactAt'] = now
            elif action == 'probeAttempt':
                if lease['kind'] != 'probe' or not lease['sent'] or data.get('phase') not in {'bootstrap','replay'} or data.get('retry') not in {0,1}:
                    raise ValueError('探测访问记录无效')
                sample = {'source':'probe-attempt','phase':data['phase'],'retry':data['retry'],'status':data.get('status'),
                          'stateReturned':data.get('stateReturned') is True,'classification':None,'durationMs':data.get('durationMs'),
                          'plannedRestSeconds':pair.get('probeRestSeconds'),'observedIdleSeconds':pair.get('observedIdleSeconds'),
                          'firstContactAt':pair.get('firstContactAt'),'previousContactAt':pair.get('previousContactAt'),
                          'lastContactAt':pair.get('lastContactAt'),'completedAt':now}
                db.execute('INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)',(now,account,lease['gateway'],json.dumps(sample)))
            elif action == 'ticketResult':
                # 窗口内其他模型拿票：只记录事实并保存 state，不做 21/29 判定。
                model = self.model(data)
                if lease['kind'] != 'business' or not lease['sent']:
                    raise ValueError('拿票记录无效')
                sample = {'source': 'ticket', 'model': model, 'status': data.get('status'), 'stateReturned': data.get('stateReturned') is True,
                          'classification': None, 'durationMs': data.get('durationMs'), 'firstContactAt': pair.get('firstContactAt'),
                          'lastContactAt': pair.get('lastContactAt'), 'completedAt': now}
                db.execute('INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)', (now, account, lease['gateway'], json.dumps(sample)))
                material = data.get('material')
                if material is not None and pair['state'] == 'ready' and pair.get('generation') == lease['generation']:
                    self.check_material(material, now)
                    db.execute('INSERT OR REPLACE INTO model_tickets VALUES (?,?,?,?,?)', (account, lease['generation'], model, material['expires'], json.dumps(material)))
            elif action in {'probeResult', 'businessResult'}:
                if (action == 'probeResult') != (lease['kind'] == 'probe'):
                    raise ValueError('租约类型不匹配')
                verdict = policy.classify(data.get('status'), data.get('stateReturned') is True, paired=data.get('paired') is True,
                                          gateway_matches=data.get('gatewayMatches') is True, failed=data.get('failed') is True)
                if verdict is not None and not lease['sent']:
                    raise ValueError('未发送请求不能判定')
                sample = {'source': lease['kind'], 'model': self.model(data) if data.get('model') is not None else None, 'status': data.get('status'), 'stateReturned': data.get('stateReturned') is True, 'classification': verdict,
                          'durationMs': data.get('durationMs'), 'plannedRestSeconds': pair.get('probeRestSeconds') if lease['kind'] == 'probe' else None,
                          'observedIdleSeconds': pair.get('observedIdleSeconds') if lease['kind'] == 'probe' else None}
                sample.update(gatewayMatches=data.get('gatewayMatches') is True, firstContactAt=pair.get('firstContactAt'),
                              previousContactAt=pair.get('previousContactAt'), lastContactAt=pair.get('lastContactAt'),
                              preparationStartedAt=pair.get('preparationStartedAt'), completedAt=now)
                db.execute('INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)', (now, account, lease['gateway'], json.dumps(sample)))
                if pair.get('generation') == lease['generation']:
                    if lease['kind'] == 'probe':
                        if verdict == '21' and pair['gateway'] in settings['gateways'] and pair['state'] == 'preparing':
                            material = data.get('material')
                            if material is not None:
                                self.check_material(material, now)
                                db.execute('INSERT OR REPLACE INTO materials VALUES (?,?,?,?)', (account, lease['generation'], material['expires'], json.dumps(material)))
                            policy.recover(pair, settings, pair.get('probeRestSeconds'))
                            previous = db.execute('SELECT data FROM pairs WHERE account=? AND gateway<>?', (account, lease['gateway'])).fetchall()
                            previous_pair = None
                            for row in previous:
                                old = json.loads(row['data'])
                                if old['state'] == 'ready':
                                    previous_pair = old
                                    policy.retire(old, settings, degraded=False)
                                    policy.settle(old)
                                    self.save(db, old)
                            if previous_pair is None:
                                # 29 先退役旧组合，探测通过时旧状态已 cooling；切换来源仍是上一已发布组合。
                                last_switch = db.execute('SELECT data FROM switches WHERE account=? ORDER BY id DESC LIMIT 1', (account,)).fetchone()
                                if last_switch:
                                    old_gateway = json.loads(last_switch['data'])['toGateway']
                                    previous_pair = self.pair(db, account, old_gateway)
                                    # 同一网关静置后恢复也保留旧代次的窗口起点，而不是本轮 preparing 的起点。
                                    prior = db.execute("SELECT data FROM observations WHERE account=? AND gateway=? AND at<? AND json_extract(data,'$.firstContactAt') IS NOT NULL ORDER BY id DESC LIMIT 1", (account, old_gateway, pair['preparationStartedAt'])).fetchone()
                                    if prior:
                                        previous_pair = {**previous_pair, 'firstContactAt':json.loads(prior['data'])['firstContactAt']}
                            switch = {'fromGateway': previous_pair['gateway'] if previous_pair else None, 'toGateway': pair['gateway'],
                                      'previousWindowSeconds': now-previous_pair['firstContactAt'] if previous_pair and previous_pair.get('firstContactAt') is not None else None,
                                      'preparationSeconds': now-pair['preparationStartedAt']}
                            db.execute('INSERT INTO switches(at,account,data) VALUES (?,?,?)', (now, account, json.dumps(switch)))
                            pair.update(state='ready', verifiedAt=now, restUntil=None)
                        else:
                            # 检测失败与 29 分开留痕，两者均按最后一次访问重新退避。
                            policy.retire(pair, settings, degraded=verdict == '29', failed=verdict is None)
                            if not lease['sent']:
                                # 池服务失败没触及上游，不重置真实静置；短暂退让避免反复抓同一候选。
                                pair.update(state='cooling', restUntil=now+5, restSeconds=pair.get('probeRestSeconds') or policy.initial(settings,pair.get('learnedRestSeconds')))
                    elif verdict == '29' or data.get('gatewayMatches') is False:
                        policy.retire(pair, settings, degraded=verdict == '29')
                self.save(db, pair)
                return {'accepted': True, 'classification': verdict}
            elif action == 'finish':
                pair['inFlight'] = max(0, pair['inFlight']-1)
                if lease['sent']:
                    pair['lastContactAt'] = max(pair.get('lastContactAt', 0), now)
                db.execute('DELETE FROM leases WHERE id=?', (lease['id'],))
            elif action == 'invalidate':
                policy.retire(pair, settings, degraded=False)
            else:
                raise ValueError('运行操作无效')
            policy.settle(pair)
            self.save(db, pair)
            return {'accepted': True}

    @staticmethod
    def model(data):
        model = data.get('model')
        if not isinstance(model, str) or not re.fullmatch(r'[a-z0-9.-]{1,64}', model):
            raise ValueError('模型标识无效')
        return model

    @staticmethod
    def check_material(material, now):
        if not isinstance(material, dict) or set(material) != {'state','cookie','expires'} or any(not isinstance(material[k], str) or not 1 <= len(material[k]) <= 16384 or '\r' in material[k] or '\n' in material[k] for k in ('state','cookie')) or not isinstance(material['expires'], (float,int)) or not now < material['expires'] <= now+86400*365:
            raise ValueError('配套材料无效')

    def view(self, account=None, gateway=None, limit=100, before=None):
        limit = min(500, max(1, int(limit)))
        # 停用也能读历史；冷却记录不随插件卸载删除。
        with self.db() as db:
            row = db.execute('SELECT data FROM settings WHERE id=1').fetchone()
            settings = json.loads(row['data']) if row else {}
            settings['poolTokenConfigured'] = bool(settings.pop('poolToken', None))
            where, args = [], []
            if account:
                where.append('account=?'); args.append(account)
            if gateway:
                where.append('gateway=?'); args.append(gateway)
            pair_where = ' WHERE ' + ' AND '.join(where) if where else ''
            pairs = [json.loads(r['data']) for r in db.execute('SELECT data FROM pairs'+pair_where+' ORDER BY account,gateway', args)]
            # 只返回已拿到票的模型名，不返回 state 或 Cookie。
            for pair in pairs:
                pair['ticketModels'] = [r['model'] for r in db.execute('SELECT model FROM model_tickets WHERE account=? AND generation=? AND expires>? ORDER BY model', (pair['accountId'], pair.get('generation'), self.clock()))] if pair['state'] == 'ready' else []
            if before is not None:
                where.append('id<?'); args.append(int(before))
            sample_where = ' WHERE ' + ' AND '.join(where) if where else ''
            samples = [{**json.loads(r['data']), **{k:r[k] for k in ('id','at','account','gateway')}} for r in db.execute('SELECT * FROM observations'+sample_where+' ORDER BY id DESC LIMIT ?', [*args,limit])]
            switch_where, switch_args = [], []
            if account:
                switch_where.append('account=?'); switch_args.append(account)
            if gateway:
                switch_where.append("(json_extract(data,'$.fromGateway')=? OR json_extract(data,'$.toGateway')=?)"); switch_args.extend([gateway,gateway])
            condition = ' WHERE '+' AND '.join(switch_where) if switch_where else ''
            switches = [{**json.loads(r['data']), 'at':r['at'], 'accountId':r['account']} for r in db.execute('SELECT * FROM switches'+condition+' ORDER BY id DESC LIMIT ?', [*switch_args,limit])]
            return {'settings': settings, 'pairs': pairs, 'observations': samples, 'switches': switches}

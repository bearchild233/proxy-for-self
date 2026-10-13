"""网关池历史事实。调度仍由 gateway_pool/policy 拥有，历史失败不得回滚租约。"""
import json
import logging
import math

LOG = logging.getLogger(__name__)
IDLE_SECONDS = 7200


def install(db, now):
    db.executescript('''
        CREATE TABLE IF NOT EXISTS gp_history_meta (id INTEGER PRIMARY KEY CHECK(id=1), started REAL NOT NULL, gaps INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS gp_batches (id TEXT PRIMARY KEY, account TEXT NOT NULL, number INTEGER NOT NULL, started REAL NOT NULL, ended REAL, status TEXT NOT NULL, candidates TEXT NOT NULL, processed TEXT NOT NULL, reason TEXT, UNIQUE(account,number));
        CREATE TABLE IF NOT EXISTS gp_windows (id TEXT PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, batch TEXT NOT NULL, started REAL NOT NULL, ended REAL, data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS gp_windows_account_time ON gp_windows(account,started);
        CREATE INDEX IF NOT EXISTS gp_windows_time ON gp_windows(started);
        CREATE INDEX IF NOT EXISTS gp_windows_batch ON gp_windows(batch);
        CREATE TABLE IF NOT EXISTS gp_accesses (id TEXT PRIMARY KEY, lease TEXT NOT NULL, generation TEXT NOT NULL, kind TEXT NOT NULL, observed REAL NOT NULL, started REAL, ended REAL, elapsed REAL, segment TEXT, outcome TEXT, status INTEGER, classification TEXT, incomplete INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS gp_accesses_lease ON gp_accesses(lease);
        CREATE INDEX IF NOT EXISTS gp_accesses_generation ON gp_accesses(generation,started);
        CREATE INDEX IF NOT EXISTS gp_accesses_segment ON gp_accesses(segment);
        CREATE TABLE IF NOT EXISTS gp_segments (id TEXT PRIMARY KEY, generation TEXT NOT NULL, started REAL NOT NULL, last_end REAL, closed REAL, reason TEXT);
        CREATE INDEX IF NOT EXISTS gp_segments_generation ON gp_segments(generation,started);
        CREATE INDEX IF NOT EXISTS gp_segments_open ON gp_segments(closed,last_end);
        CREATE TABLE IF NOT EXISTS gp_cooling (id INTEGER PRIMARY KEY, account TEXT NOT NULL, gateway TEXT NOT NULL, started REAL NOT NULL, recovered REAL, data TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS gp_cooling_pair ON gp_cooling(account,gateway,recovered);
        CREATE INDEX IF NOT EXISTS gp_cooling_time ON gp_cooling(started);
        CREATE INDEX IF NOT EXISTS gp_cooling_pair_recent ON gp_cooling(account,gateway,id);
        CREATE INDEX IF NOT EXISTS observations_generation ON observations(json_extract(data,'$.generation'),id);
    ''')
    db.execute('INSERT OR IGNORE INTO gp_history_meta(id,started) VALUES (1,?)', (now,))


def record(db, operation, *args):
    # 保存点仅包住新增历史写入，失败时仍提交调用方已经完成的运行状态变更。
    db.execute('SAVEPOINT pool_history')
    try:
        operation(db, *args)
    except Exception as error:
        db.execute('ROLLBACK TO pool_history')
        LOG.warning('gateway pool history unavailable: %s / %s', operation.__name__, type(error).__name__)
        try:
            db.execute('UPDATE gp_history_meta SET gaps=gaps+1 WHERE id=1')
        except Exception:
            pass
    finally:
        db.execute('RELEASE pool_history')


def write_window(db, item):
    db.execute('INSERT OR REPLACE INTO gp_windows VALUES (?,?,?,?,?,?,?)',
               (item['id'], item['accountId'], item['gateway'], item['batchId'], item['preparationStartedAt'], item['drainedAt'], json.dumps(item)))


def begin(db, pair, gateways, now):
    previous = db.execute('SELECT * FROM gp_batches WHERE account=? ORDER BY number DESC LIMIT 1', (pair['accountId'],)).fetchone()
    if previous is None or previous['status'] != 'active':
        batch_id, number = pair['generation'], previous['number']+1 if previous else 1
        db.execute('INSERT INTO gp_batches VALUES (?,?,?,?,NULL,?,?,?,NULL)',
                   (batch_id, pair['accountId'], number, now, 'active', json.dumps(sorted(set(gateways))), '{}'))
    else:
        batch_id, number = previous['id'], previous['number']
    write_window(db, dict(id=pair['generation'], accountId=pair['accountId'], gateway=pair['gateway'], batchId=batch_id,
                         batchNumber=number, preparationStartedAt=now, probeEndedAt=None, probeResult=None,
                         firstContactAt=None, verifiedAt=None, retiredAt=None, drainedAt=None, state='preparing',
                         reason=None, incomplete=False))


def interrupt(db, now, reason):
    db.execute("UPDATE gp_batches SET status='interrupted',ended=?,reason=? WHERE status='active'", (now, reason))


def close_idle(db, now, generation=None):
    # 未回报起点的发送许可也阻止提前结算，不能把收头前的请求当成空闲。
    clause, args = (' AND s.generation=?', [generation]) if generation else ('', [])
    db.execute('''UPDATE gp_segments AS s SET closed=last_end+?,reason='idle'
        WHERE closed IS NULL AND last_end<=?''' + clause + '''
        AND NOT EXISTS (SELECT 1 FROM gp_accesses a JOIN leases l ON l.id=a.lease
            WHERE a.generation=s.generation AND a.kind IN ('business','legacy') AND a.ended IS NULL AND l.expires>?)''',
        [IDLE_SECONDS, now-IDLE_SECONDS, *args, now])


def sync(db, pair, now):
    row = db.execute('SELECT data FROM gp_windows WHERE id=?', (pair.get('generation'),)).fetchone()
    if not row:
        return
    item = json.loads(row['data'])
    previous = item['state']
    item['state'] = pair['state']
    if pair['state'] == 'ready' and item['verifiedAt'] is None:
        item['verifiedAt'] = pair.get('verifiedAt', now)
        cycle = db.execute('SELECT * FROM gp_cooling WHERE account=? AND gateway=? ORDER BY id DESC LIMIT 1', (pair['accountId'],pair['gateway'])).fetchone()
        if cycle:
            data = json.loads(cycle['data'])
            step = data['steps'][-1]
            if step['generation'] == item['id'] and step['result'] == '21':
                step['learnedRestSeconds'] = pair.get('learnedRestSeconds')
                db.execute('UPDATE gp_cooling SET data=? WHERE id=?', (json.dumps(data),cycle['id']))
    if pair['state'] in {'draining', 'cooling', 'unknown'} and item['retiredAt'] is None:
        item.update(retiredAt=now, reason=pair.get('retiredReason', 'rotation'))
        db.execute('UPDATE gp_segments SET closed=?,reason=? WHERE generation=? AND closed IS NULL', (now, item['reason'], item['id']))
    if item['retiredAt'] is not None and not pair.get('inFlight') and item['drainedAt'] is None:
        accesses = db.execute('SELECT MAX(ended) AS ended,MAX(incomplete) AS missing FROM gp_accesses WHERE generation=?', (item['id'],)).fetchone()
        item['drainedAt'] = max(item['retiredAt'],accesses['ended']) if accesses['ended'] is not None and not accesses['missing'] and not item['incomplete'] else now
    write_window(db, item)
    if item['drainedAt'] is not None:
        batch = db.execute('SELECT * FROM gp_batches WHERE id=?', (item['batchId'],)).fetchone()
        processed = json.loads(batch['processed'])
        # 未触及上游的 Cookie/目录失败不算遍历，重复候选仍留在当前批次。
        if item['firstContactAt'] is not None:
            processed[item['gateway']] = 'served' if item['verifiedAt'] is not None else item['probeResult'] or 'interrupted'
        db.execute('UPDATE gp_batches SET processed=? WHERE id=?', (json.dumps(processed), batch['id']))
        if batch['status'] == 'active' and set(processed) >= set(json.loads(batch['candidates'])):
            if not db.execute('SELECT 1 FROM gp_windows WHERE batch=? AND ended IS NULL LIMIT 1', (batch['id'],)).fetchone():
                db.execute("UPDATE gp_batches SET status='complete',ended=? WHERE id=?", (now, batch['id']))
    if pair['state'] == 'cooling' and previous != 'cooling' and pair.get('lastContactAt') is not None and db.execute('SELECT 1 FROM gp_accesses WHERE generation=? LIMIT 1', (item['id'],)).fetchone():
        cycle = db.execute('SELECT * FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1',
                           (pair['accountId'], pair['gateway'])).fetchone()
        data = json.loads(cycle['data']) if cycle else {'reason': item['reason'], 'steps': []}
        if data['steps'] and data['steps'][-1]['scheduledStartAt'] == pair['lastContactAt']:
            return
        last = db.execute('SELECT MAX(ended) AS ended,MAX(incomplete) AS missing,COUNT(*) AS count FROM gp_accesses WHERE generation=?', (item['id'],)).fetchone()
        # 调度的保守时间与准确访问时间分别保存，失联回收不会伪装成准确静置。
        actual = last['ended'] if last['count'] and not last['missing'] and not item['incomplete'] else None
        data['steps'].append(dict(generation=None, startedAt=actual, scheduledStartAt=pair['lastContactAt'], plannedSeconds=pair.get('restSeconds'),
                                  dueAt=pair.get('restUntil'), probeAt=None, resultAt=None, result=None, status=None,
                                  learnedRestSeconds=pair.get('learnedRestSeconds')))
        if cycle:
            db.execute('UPDATE gp_cooling SET data=? WHERE id=?', (json.dumps(data), cycle['id']))
        else:
            db.execute('INSERT INTO gp_cooling(account,gateway,started,data) VALUES (?,?,?,?)',
                       (pair['accountId'], pair['gateway'], actual if actual is not None else pair['lastContactAt'], json.dumps(data)))


def sent(db, lease, payload, now):
    kind = payload.get('purpose')
    if kind not in {'business','ticket','bootstrap','replay'}:
        kind = 'legacy'
    access_id = payload.get('accessId') if payload.get('timingVersion') == 2 else None
    if not isinstance(access_id, str) or not 1 <= len(access_id) <= 80:
        access_id = 'legacy:' + lease['id']
        kind = 'legacy'
    close_idle(db, now, lease['generation'])
    db.execute('INSERT OR IGNORE INTO gp_accesses(id,lease,generation,kind,observed,incomplete) VALUES (?,?,?,?,?,?)',
               (access_id, lease['id'], lease['generation'], kind, now, int(kind == 'legacy')))


def report(db, lease, payload, now):
    access = payload.get('access')
    if not isinstance(access, dict):
        return
    row = db.execute('SELECT * FROM gp_accesses WHERE id=? AND lease=?', (access.get('id'), lease['id'])).fetchone()
    if row is None or row['ended'] is not None:
        return
    start, elapsed = access.get('startedAt'), access.get('elapsedMs')
    if any(isinstance(v, bool) or not isinstance(v, (int,float)) or not math.isfinite(v) for v in (start,elapsed)):
        raise ValueError('访问计时无效')
    if not 0 <= elapsed <= 86400000 or not row['observed']-5 <= start <= now+5 or start+elapsed/1000 > now+5:
        raise ValueError('访问计时超出范围')
    end = start+elapsed/1000 if access.get('ended') is True else None
    outcome = access.get('outcome')
    if outcome not in {'completed','failed','cancelled','interrupted','rejected','headers'}:
        outcome = 'interrupted'
    segment = row['segment']
    if row['kind'] == 'business' and not segment:
        previous = db.execute('SELECT * FROM gp_segments WHERE generation=? AND (closed IS NULL OR closed>?) ORDER BY started DESC LIMIT 1', (lease['generation'],start)).fetchone()
        if previous:
            segment = previous['id']
            db.execute('UPDATE gp_segments SET started=MIN(started,?) WHERE id=?', (start,segment))
        else:
            segment = row['id']
            window = db.execute('SELECT data FROM gp_windows WHERE id=?', (lease['generation'],)).fetchone()
            item = json.loads(window['data']) if window else {}
            db.execute('INSERT INTO gp_segments VALUES (?,?,?,NULL,?,?)', (segment,lease['generation'],start,item.get('retiredAt'),item.get('reason')))
    db.execute('UPDATE gp_accesses SET started=?,ended=?,elapsed=?,segment=?,outcome=?,status=COALESCE(?,status),classification=COALESCE(?,classification) WHERE id=?',
               (start,end,elapsed/1000,segment,outcome,payload.get('status'),payload.get('classification'),row['id']))
    if segment and end is not None:
        db.execute('UPDATE gp_segments SET last_end=MAX(COALESCE(last_end,0),?) WHERE id=?', (end,segment))
    window = db.execute('SELECT data FROM gp_windows WHERE id=?', (lease['generation'],)).fetchone()
    if window:
        item = json.loads(window['data'])
        item['firstContactAt'] = min(item['firstContactAt'],start) if item['firstContactAt'] is not None else start
        write_window(db,item)
    if lease['kind'] == 'probe':
        cycle = db.execute('SELECT * FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1', (lease['account'],lease['gateway'])).fetchone()
        if cycle:
            data = json.loads(cycle['data'])
            step = data['steps'][-1]
            if step['probeAt'] is None:
                step.update(probeAt=start,generation=lease['generation'])
                db.execute('UPDATE gp_cooling SET data=? WHERE id=?', (json.dumps(data),cycle['id']))


def finish(db, lease, payload, now, incomplete=False):
    report(db,lease,payload,now)
    db.execute('UPDATE gp_accesses SET incomplete=1,outcome=COALESCE(outcome,?) WHERE lease=? AND ended IS NULL', ('interrupted',lease['id']))
    missing = incomplete or db.execute('SELECT 1 FROM gp_accesses WHERE lease=? AND incomplete=1 LIMIT 1', (lease['id'],)).fetchone()
    if missing:
        row = db.execute('SELECT data FROM gp_windows WHERE id=?', (lease['generation'],)).fetchone()
        if row:
            item = json.loads(row['data']); item['incomplete'] = True; write_window(db,item)


def result(db, lease, payload, verdict, now):
    report(db,lease,{**payload,'classification':verdict},now)
    if lease['kind'] != 'probe':
        return
    row = db.execute('SELECT data FROM gp_windows WHERE id=?', (lease['generation'],)).fetchone()
    if row:
        item = json.loads(row['data'])
        item.update(probeEndedAt=now,probeResult=verdict or 'failed')
        duration = payload.get('durationMs')
        if isinstance(duration,(int,float)) and not isinstance(duration,bool) and math.isfinite(duration) and 0 <= duration <= 60000:
            item['preparationSeconds'] = duration/1000
            measured = payload.get('preparationStartedAt')
            if isinstance(measured,(int,float)) and not isinstance(measured,bool) and math.isfinite(measured) and item['preparationStartedAt']-5 <= measured <= now:
                item.update(preparationMeasuredAt=measured,probeEndedAt=measured+duration/1000)
        # 旧网关仅掌握许可时间，历史窗口保留缺口，不伪造实际访问时间。
        if lease['sent'] and item['firstContactAt'] is None:
            item['incomplete'] = True
        write_window(db,item)
    cycle = db.execute('SELECT * FROM gp_cooling WHERE account=? AND gateway=? AND recovered IS NULL ORDER BY id DESC LIMIT 1', (lease['account'],lease['gateway'])).fetchone()
    if cycle and lease['sent']:
        data = json.loads(cycle['data'])
        step = data['steps'][-1]
        if step['resultAt'] is None:
            last = db.execute('SELECT MAX(ended) FROM gp_accesses WHERE generation=?', (lease['generation'],)).fetchone()[0]
            observed = last if last is not None else now
            step.update(resultAt=observed,result=verdict or 'failed',status=payload.get('status'))
            db.execute('UPDATE gp_cooling SET data=?,recovered=? WHERE id=?', (json.dumps(data),observed if verdict == '21' else None,cycle['id']))

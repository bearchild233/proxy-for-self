"""网关池共用账号队列。与运行租约共用事务，排序不改变在途请求归属。"""
import json
import re


def install(db):
    db.executescript('''
        CREATE TABLE IF NOT EXISTS gp_account_queue (
            account TEXT PRIMARY KEY, position INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            quota_exhausted INTEGER NOT NULL DEFAULT 0, remaining TEXT, reason TEXT);
        CREATE TABLE IF NOT EXISTS gp_queue_revision (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        INSERT OR IGNORE INTO gp_queue_revision VALUES (1,0);
    ''')


def identifiers(values):
    if not isinstance(values,list) or len(values)>512 or len(set(v for v in values if isinstance(v,str)))!=len(values) or any(not isinstance(v,str) or not re.fullmatch(r'[A-Za-z0-9:_-]{1,128}',v) for v in values):
        raise ValueError('账号列表无效')
    return values


def changed(db):
    db.execute('UPDATE gp_queue_revision SET revision=revision+1 WHERE id=1')


def register(db, accounts):
    for account in identifiers(accounts):
        if db.execute('INSERT OR IGNORE INTO gp_account_queue(account,position) VALUES (?,(SELECT COALESCE(MAX(position),0)+1 FROM gp_account_queue))',(account,)).rowcount:
            changed(db)


def view(db):
    return dict(revision=db.execute('SELECT revision FROM gp_queue_revision WHERE id=1').fetchone()[0],
                accounts=[dict(accountId=r['account'],enabled=bool(r['enabled']),quotaExhausted=bool(r['quota_exhausted']),reason=r['reason'])
                          for r in db.execute('SELECT * FROM gp_account_queue ORDER BY position,account')])


def move_last(db, account, reason):
    db.execute('UPDATE gp_account_queue SET position=(SELECT COALESCE(MAX(position),0)+1 FROM gp_account_queue),remaining=NULL,reason=? WHERE account=?',(reason,account))
    changed(db)


def update(db, data):
    if not isinstance(data,dict) or set(data)-{'revision','accountIds','accountId','enabled'}:
        raise ValueError('账号队列参数无效')
    if type(data.get('revision')) is not int or data['revision']!=view(db)['revision']:
        raise ValueError('账号队列已变化，请刷新后重试')
    if 'accountIds' in data:
        if 'accountId' in data or 'enabled' in data:
            raise ValueError('不能同时排序和切换账号')
        ids=identifiers(data['accountIds']);register(db,ids)
        # 只重排当前可见账号，已离开分组的历史账号保留原位置。
        slots=sorted(r[0] for r in db.execute('SELECT position,account FROM gp_account_queue') if r[1] in ids)
        for account,position in zip(ids,slots):
            db.execute('UPDATE gp_account_queue SET position=? WHERE account=?',(position,account))
    else:
        account=identifiers([data.get('accountId')])[0]
        if not isinstance(data.get('enabled'),bool):
            raise ValueError('账号开关无效')
        register(db,[account])
        db.execute('UPDATE gp_account_queue SET enabled=? WHERE account=?',(data['enabled'],account))
    changed(db)
    return view(db)


def allowed(db, account):
    row=db.execute('SELECT enabled,quota_exhausted FROM gp_account_queue WHERE account=?',(account,)).fetchone()
    return row is None or bool(row['enabled'] and not row['quota_exhausted'])


def quota(db, account, exhausted):
    register(db,[account])
    row=db.execute('SELECT quota_exhausted FROM gp_account_queue WHERE account=?',(account,)).fetchone()
    if bool(row[0])!=exhausted:
        db.execute('UPDATE gp_account_queue SET quota_exhausted=? WHERE account=?',(exhausted,account))
        if exhausted:
            move_last(db,account,'quota')
        else:
            changed(db)


def remaining(db, account):
    row=db.execute('SELECT remaining FROM gp_account_queue WHERE account=?',(account,)).fetchone()
    return json.loads(row[0]) if row and row[0] is not None else None


def begin(db, account, gateways):
    register(db,[account])
    db.execute('UPDATE gp_account_queue SET remaining=? WHERE account=? AND remaining IS NULL',(json.dumps(gateways),account))


def retired(db, pair, previous):
    if not allowed(db,pair['accountId']) or previous not in {'ready','preparing'} or pair['state'] not in {'draining','cooling'} or pair.get('firstContactAt') is None:
        return
    pending=remaining(db,pair['accountId'])
    if pending is None or pair['gateway'] not in pending:
        return
    pending.remove(pair['gateway'])
    if pending:
        db.execute('UPDATE gp_account_queue SET remaining=? WHERE account=?',(json.dumps(pending),pair['accountId']))
    else:
        move_last(db,pair['accountId'],'gateways')

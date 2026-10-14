#!/usr/bin/env python3.10
"""网关池存储、设置与管理读取的一致性对照：用 Python 实现生成期望，供 Rust 插件重放（见同目录 README.md）。
依赖 services/plugin_host 里的 Python 实现，P2 删除 Python 时随之删除。

用法：
  gen_python.py generate <仓库路径> <产物目录>          # 用 Python 实现生成期望输出与快照库
  gen_python.py verify-rust <仓库路径> <产物目录>       # 读取 Rust 写入的库，比较 Python 输出与 Rust 期望

期望输出由 Python 实现（services/plugin_host/gateway_pool.py）产生；比较时使用严格的 JSON 文本比较，
整数与浮点的区别不会被忽略。
"""
import json
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path

REPO = Path(sys.argv[2] if len(sys.argv) > 2 else '.').resolve()
sys.path.insert(0, str(REPO / 'services/plugin_host'))

from gateway_pool import GatewayPool  # noqa: E402
import test_gateway_pool as base  # noqa: E402  (Store 桩与策略文件加载)
import test_gateway_pool_history as hist  # noqa: E402

FIXED_CLOCK = 10000.0


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False)


def py_call(pool, method, data):
    """与 server.py 的 gateway-pool.* 分支一致的 Python 侧调用。"""
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


def new_pool(root, clock):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    store = base.Store(root)
    return GatewayPool(root, store, lambda: clock[0]), root


def raw_sql(pool, sql, args):
    with pool.db() as db:
        db.execute(sql, args)
    return {'ok': None}


def last_revision(pool):
    return pool.view()['accountQueue']['revision']


def schema_of(path):
    """表、索引的结构摘要：类型、名称、归属表、规范化后的 DDL、列与索引列。"""
    connection = sqlite3.connect(path)
    try:
        entries = []
        rows = connection.execute(
            "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetchall()
        for type_, name, table, sql in rows:
            entry = {'type': type_, 'name': name, 'tableName': table, 'sql': ' '.join((sql or '').split())}
            if type_ == 'table':
                entry['columns'] = [list(row) for row in connection.execute('PRAGMA table_info("%s")' % name)]
            if type_ == 'index':
                entry['indexColumns'] = [list(row) for row in connection.execute('PRAGMA index_info("%s")' % name)]
            entries.append(entry)
        return entries
    finally:
        connection.close()


def write_steps(pool, clock, steps_spec):
    """执行写路径步骤，返回带期望输出的步骤列表。数据中的 revision 由当前队列视图取得。"""
    steps = []
    for spec in steps_spec:
        if 'clock' in spec:
            clock[0] = spec['clock']
        if spec['op'] == 'sql':
            expect = raw_sql(pool, spec['sql'], spec.get('args', []))
            steps.append({'op': 'sql', 'sql': spec['sql'], 'args': spec.get('args', []), 'clock': clock[0], 'expect': expect})
            continue
        data = spec['data'](pool) if callable(spec['data']) else spec['data']
        expect = py_call(pool, spec['method'], data)
        steps.append({'op': 'call', 'method': spec['method'], 'data': data, 'clock': clock[0], 'expect': expect})
    return steps


def fresh_case(name, steps_spec, clock_value=FIXED_CLOCK):
    temp = tempfile.mkdtemp(prefix='gp-parity-')
    clock = [clock_value]
    pool, _ = new_pool(temp, clock)
    steps = write_steps(pool, clock, steps_spec)
    shutil.rmtree(temp, ignore_errors=True)
    return {'name': name, 'steps': steps}


INVALID_CONFIGURE = [
    {'probeIntervalMs': 50.5}, {'windowSeconds': True}, {'initialRestSeconds': 90000},
    {'maximumRestSeconds': 1, 'initialRestSeconds': 2}, {'mode': 'sometimes'},
    {'poolUrl': 'http://example.com'}, {'poolUrl': 'https://u:p@example.com'},
    {'poolUrl': 'https://example.com/path'}, {'poolUrl': 'https://example.com/?x=1'},
    {'poolUrl': 'https://'}, {'gateways': ['bad']}, {'groupIds': ['bad id']},
    {'poolToken': 'a\nb'}, {'poolToken': 3}, {'unknown': 1}, {'probeModel': 'claude-x'},
    {'poolDirect': 'yes'}, {'dynamicGroups': 'yes'}, {'normalGroupId': 'bad id'},
    {'dynamicGroups': True}, {'dynamicGroups': True, 'normalGroupId': 'g1'},
    {'dynamicGroups': True, 'normalGroupId': 'g1', 'degradedGroupId': 'g1', 'groupIds': ['g1']},
    {'dynamicGroups': True, 'normalGroupId': 'g1', 'degradedGroupId': 'g2', 'groupIds': ['g1']},
    {'poolAccountIds': {'A': 'member\ninvalid'}}, {'poolAccountIds': []},
    {'gateways': ['unified-142', 'unified-142']}, {'groupIds': 'g1'},
]

VALID_CONFIGURE = [
    {'gateways': ['unified-142', 'unified-143'], 'initialRestSeconds': 3600},
    {'poolToken': 'fake-pool-secret'},
    {'probeIntervalMs': 50}, {'windowSeconds': 1.5}, {'recoverySeconds': 480},
    {'mode': 'continuous', 'poolDirect': False, 'probeModel': 'gpt-6-astra'},
    {'poolUrl': 'https://pool.example.com:8443'},
    {'poolUrl': 'HTTPS://pool.example.com?'},
    {'groupIds': ['g1', 'g2'], 'dynamicGroups': True, 'normalGroupId': 'g1', 'degradedGroupId': 'g2'},
    {'poolAccountIds': {'A': 'gwpool-member:account-uuid/user.member_1'}},
]


def settings_case():
    steps = [{'op': 'call', 'method': 'read', 'data': {}}]
    steps += [{'op': 'call', 'method': 'configure', 'data': data} for data in VALID_CONFIGURE]
    steps += [{'op': 'call', 'method': 'configure', 'data': data} for data in INVALID_CONFIGURE]
    steps += [
        {'op': 'call', 'method': 'read', 'data': {}},
        {'op': 'call', 'method': 'read', 'data': {'accountId': 'A'}},
        {'op': 'call', 'method': 'read', 'data': {'limit': 2.7}},
        {'op': 'call', 'method': 'read', 'data': {'limit': '3'}},
        {'op': 'call', 'method': 'read', 'data': {'limit': 'x'}},
        {'op': 'call', 'method': 'read', 'data': {'before': 5}},
        {'op': 'call', 'method': 'read', 'data': {'accountId': 0, 'gateway': False}},
    ]
    return fresh_case('settings-and-read', steps)


def queue_case():
    def revision_of(pool):
        return last_revision(pool)

    steps = [
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountIds': ['A', 'B', 'C']}},
        {'op': 'call', 'method': 'read', 'data': {}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountIds': ['C', 'A', 'B']}},
        {'op': 'call', 'method': 'queue.update', 'data': {'revision': 0, 'accountIds': ['A']}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountId': 'B', 'enabled': False}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountId': 'B', 'enabled': 'x'}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountIds': ['A', 'A']}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'accountIds': ['C'], 'enabled': True}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': 1.0, 'accountIds': ['A']}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': revision_of(p), 'extra': 1}},
        {'op': 'call', 'method': 'read', 'data': {}},
    ]
    return fresh_case('queue-flow', steps)


def seeded_case():
    pair = lambda account, gateway, **extra: json.dumps({'accountId': account, 'gateway': gateway, **extra}, ensure_ascii=False)
    steps = [
        {'op': 'call', 'method': 'configure', 'data': {'gateways': ['unified-142', 'unified-143']}},
        {'op': 'sql', 'sql': 'INSERT OR REPLACE INTO pairs VALUES (?,?,?)', 'args': [
            'A', 'unified-142', pair('A', 'unified-142', state='ready', inFlight=0, generation='g1', firstContactAt=9000.0,
                                     lastContactAt=9500.0, restSeconds=3600.0, verifiedAt=9100.0, cookieExpiresAt=10600.0,
                                     retiredReason=None, probeRestSeconds=7200)]},
        {'op': 'sql', 'sql': 'INSERT OR REPLACE INTO pairs VALUES (?,?,?)', 'args': [
            'A', 'unified-143', pair('A', 'unified-143', state='preparing', inFlight=0, generation='g2', probeRestSeconds=57600,
                                     preparationStartedAt=9800.0, restSeconds=7200.0)]},
        {'op': 'sql', 'sql': 'INSERT OR REPLACE INTO pairs VALUES (?,?,?)', 'args': [
            'B', 'unified-142', pair('B', 'unified-142', state='cooling', inFlight=0, generation='g3', firstContactAt=8000.0,
                                     lastContactAt=8500.0, restSeconds=7200.0, restUntil=15700.0, learnedRestSeconds=3600.0)]},
        {'op': 'sql', 'sql': 'INSERT INTO model_tickets VALUES (?,?,?,?,?)', 'args': [
            'A', 'g1', 'gpt-6-astra', 10600.0, json.dumps({'state': 's', 'cookie': 'c', 'expires': 10600.0})]},
        {'op': 'sql', 'sql': 'INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)', 'args': [
            9100.0, 'A', 'unified-142', json.dumps({'generation': 'g1', 'source': 'probe', 'status': 200, 'classification': '21', 'stateReturned': False})]},
        {'op': 'sql', 'sql': 'INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)', 'args': [
            9200.0, 'A', 'unified-142', json.dumps({'generation': 'g1', 'source': 'business', 'status': 200, 'classification': None})]},
        {'op': 'sql', 'sql': 'INSERT INTO switches(at,account,data) VALUES (?,?,?)', 'args': [
            9100.0, 'A', json.dumps({'fromAccount': None, 'fromGateway': None, 'toGateway': 'unified-142', 'previousWindowSeconds': None, 'preparationSeconds': 12.5})]},
        {'op': 'sql', 'sql': 'INSERT INTO catalogs VALUES (?,?,?)', 'args': [
            'A', 9000.0, json.dumps([{'gateway': 'unified-142', 'validForS': 600, 'country': 'CH'}])]},
        {'op': 'sql', 'sql': 'INSERT INTO group_events VALUES (?,?,?,?,?,?,?,?,?)', 'args': [
            'a' * 32, 9300.0, 'A', 'degraded', 'g1', 'g2', 'gpt-6.1-sol', 'moved', 9301.0]},
        {'op': 'call', 'method': 'read', 'data': {}},
        {'op': 'call', 'method': 'read', 'data': {'accountId': 'A'}},
        {'op': 'call', 'method': 'read', 'data': {'accountId': 'B', 'gateway': 'unified-142', 'limit': 1}},
        {'op': 'call', 'method': 'read', 'data': {'gateway': 'unified-999'}},
        {'op': 'call', 'method': 'read', 'data': {'accountId': 'A', 'before': 9200}},
        {'op': 'call', 'method': 'configure', 'data': {'gateways': ['unified-142']}, 'clock': 10000.0},
        {'op': 'call', 'method': 'read', 'data': {}},
        {'op': 'call', 'method': 'queue.update', 'data': lambda p: {'revision': last_revision(p), 'accountId': 'A', 'enabled': False}},
        {'op': 'call', 'method': 'read', 'data': {}},
    ]
    return fresh_case('seeded-state', steps)


def empty_statistics_case():
    steps = [
        {'op': 'call', 'method': 'history.read', 'data': {}},
        {'op': 'call', 'method': 'history.read', 'data': {'view': 'summary'}},
        {'op': 'call', 'method': 'history.read', 'data': {'view': 'cooling', 'status': 'incomplete'}},
        {'op': 'call', 'method': 'history.read', 'data': {'view': 'records', 'page': 2, 'pageSize': 5}},
        {'op': 'call', 'method': 'history.read', 'data': {'from': 9000, 'to': 9500, 'accountId': 'A'}},
        {'op': 'call', 'method': 'history.read', 'data': {'from': 9500, 'to': 9000}},
        {'op': 'call', 'method': 'history.read', 'data': {'to': 10061}},
        {'op': 'call', 'method': 'history.read', 'data': {'view': 'cooling', 'status': 'recovered'}},
        {'op': 'call', 'method': 'history.read', 'data': {'batchId': 'b1', 'reason': 'rotation', 'view': 'summary'}},
        {'op': 'call', 'method': 'history.read', 'data': {'pageSize': 101}},
    ]
    return fresh_case('empty-statistics', steps)


def repair_synthetic_steps(path):
    """测试直接插入的合成冷却步骤缺少 dueAt（运行时总会写入）。补齐为 probeAt，使两侧读取同一份完整数据。"""
    connection = sqlite3.connect(path)
    try:
        for row_id, data in connection.execute('SELECT id,data FROM gp_cooling').fetchall():
            document = json.loads(data)
            for step in document.get('steps', []):
                if 'dueAt' not in step:
                    step['dueAt'] = step.get('probeAt')
            connection.execute('UPDATE gp_cooling SET data=? WHERE id=?', (json.dumps(document), row_id))
        connection.commit()
    finally:
        connection.close()


def snapshot_scenarios():
    """用 Python 测试中的历史场景生成真实运行数据库，并记录统计与读取的期望输出。"""
    names = [
        'test_headers_do_not_finish_business_and_overlap_is_merged',
        'test_ticket_is_separate_and_finish_timestamp_excludes_rpc_delay',
        'test_two_hour_boundary_persists_segments_without_retiring_window',
        'test_inflight_prevents_idle_closure',
        'test_repeat_candidate_stays_in_same_batch_and_restart_keeps_number',
        'test_retirement_drains_before_batch_and_cooling',
        'test_recovery_requires_actual_21_and_records_each_tier',
        'test_recovery_learning_and_no_contact_retry_keep_original_cooling',
        'test_legacy_and_expired_access_are_unknown_not_infinite_duration',
        'test_duplicate_finish_is_idempotent_and_filters_keep_full_totals',
        'test_card_read_after_due_is_passive_and_waits_for_actual_visit',
        'test_card_recent_visits_keep_account_scope_and_latest_failed_attempt',
        'test_account_cooling_analysis_uses_all_visits_and_actual_interval_bins',
        'test_cooling_analysis_filters_visit_time_and_separates_unknown_outcomes',
        'test_rotation_detail_keeps_detection_ticket_and_business_in_exact_generation',
        'test_rotation_detail_limits_visible_detection_rows_but_keeps_total',
    ]
    snapshots = []
    out_dir = OUT / 'snapshots'
    out_dir.mkdir(parents=True, exist_ok=True)
    for name in names:
        case = hist.GatewayPoolHistoryTests(name)
        case.setUp()
        try:
            getattr(case, name)()
            # 生产时钟是 time.time()，恒为浮点；测试里的整数时钟不代表生产类型，统一转为浮点。
            now = float(case.now)
            pool = case.pool
            # 检查点必须在没有打开事务的连接上执行，并截断 WAL，使主库文件自身包含全部数据。
            checkpoint = sqlite3.connect(pool.path, isolation_level=None)
            try:
                checkpoint.execute('PRAGMA wal_checkpoint(TRUNCATE)')
            finally:
                checkpoint.close()
            source = Path(pool.path)
            copied = out_dir / (name + '.sqlite3')
            shutil.copyfile(source, copied)
            repair_synthetic_steps(copied)
            snapshots.append({'name': name, 'file': 'snapshots/' + copied.name, 'now': now,
                              'steps': read_steps(copied, now)})
        finally:
            case.temp.cleanup()
    return snapshots


READ_QUERIES = [
    ('history.read', {}),
    ('history.read', {'view': 'summary'}),
    ('history.read', {'view': 'summary', 'accountId': 'A', 'pageSize': 1}),
    ('history.read', {'view': 'cooling'}),
    ('history.read', {'view': 'cooling', 'status': 'incomplete'}),
    ('history.read', {'view': 'cooling', 'status': 'due'}),
    ('history.read', {'view': 'cooling', 'status': 'cooling'}),
    ('history.read', {'view': 'records', 'pageSize': 1, 'page': 2}),
    ('history.read', {'view': 'records', 'pageSize': 3, 'page': 1, 'accountId': 'A'}),
    ('history.read', {'accountId': 'B'}),
    ('history.read', {'gateway': 'unified-143'}),
    ('history.read', {'reason': '29'}),
    ('history.read', {'view': 'summary', 'reason': 'rotation'}),
    ('history.read', {'from': 9000, 'to': 9500}),
    ('history.read', {'from': 10000, 'to': 10005.5, 'view': 'summary'}),
    ('history.read', {'to': 1e9}),
    ('read', {}),
    ('read', {'accountId': 'A', 'limit': 3}),
    ('read', {'accountId': 'A', 'gateway': 'unified-142', 'limit': 2}),
]


def read_steps(path, now):
    root = Path(tempfile.mkdtemp(prefix='gp-snapshot-'))
    try:
        target = root / 'gateway-pool-runtime'
        target.mkdir(parents=True)
        shutil.copyfile(path, target / 'state.sqlite3')
        clock = [float(now)]
        pool, _ = new_pool(root, clock)
        return [{'op': 'call', 'method': method, 'data': data, 'clock': now, 'expect': py_call(pool, method, data)}
                for method, data in READ_QUERIES]
    finally:
        shutil.rmtree(root, ignore_errors=True)


def generate():
    OUT.mkdir(parents=True, exist_ok=True)
    fresh = tempfile.mkdtemp(prefix='gp-schema-')
    clock = [FIXED_CLOCK]
    pool, root = new_pool(fresh, clock)
    schema = schema_of(root / 'gateway-pool-runtime' / 'state.sqlite3')
    shutil.rmtree(fresh, ignore_errors=True)
    cases = [settings_case(), queue_case(), seeded_case(), empty_statistics_case()]
    snapshots = snapshot_scenarios()
    document = {'schema': schema, 'cases': cases, 'snapshots': snapshots}
    (OUT / 'cases.json').write_text(json.dumps(document, ensure_ascii=False, indent=1, allow_nan=False), encoding='utf-8')
    total = sum(len(c['steps']) for c in cases) + sum(len(s['steps']) for s in snapshots)
    print('python cases: %d write-path cases, %d snapshots, %d expectations, %d schema entries' % (
        len(cases), len(snapshots), total, len(schema)))


def verify_rust():
    """Rust 写入的库由 Python 读取，并与 Rust 记录的期望逐项比较。"""
    rust_dir = OUT / 'rust-written'
    expected = json.loads((rust_dir / 'rust-expect.json').read_text(encoding='utf-8'))
    root = Path(tempfile.mkdtemp(prefix='gp-rust-to-python-'))
    try:
        target = root / 'gateway-pool-runtime'
        target.mkdir(parents=True)
        shutil.copyfile(rust_dir / 'state.sqlite3', target / 'state.sqlite3')
        clock = [expected['now']]
        pool, _ = new_pool(root, clock)
        results = []
        mismatches = 0
        for step in expected['steps']:
            # 只比较读取：写入步骤已在 Python 侧的写入对照中验证，重放会改变状态。
            if step['method'] not in ('read', 'history.read'):
                continue
            clock[0] = step['clock']
            actual = py_call(pool, step['method'], step['data'])
            same = canonical(actual) == canonical(step['expect'])
            mismatches += 0 if same else 1
            results.append({'method': step['method'], 'data': step['data'], 'match': same})
        report = {'steps': len(results), 'mismatches': mismatches, 'results': results}
        (OUT / 'rust-to-python-report.json').write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding='utf-8')
        print('rust-to-python: %d reads compared, %d mismatches' % (len(results), mismatches))
        return 0 if mismatches == 0 else 1
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == '__main__':
    command = sys.argv[1]
    if len(sys.argv) < 4:
        raise SystemExit('usage: gen_python.py generate|verify-rust <仓库路径> <产物目录>')
    OUT = Path(sys.argv[3]).resolve()
    if command == 'generate':
        generate()
    elif command == 'verify-rust':
        sys.exit(verify_rust())
    else:
        raise SystemExit('unknown command')

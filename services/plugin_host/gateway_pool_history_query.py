"""网关池历史只读投影。完整范围聚合与当前页分开，未知时间不补零。"""
import json
import math
from collections import defaultdict
from statistics import median

METRICS = ('preparationSeconds', 'windowSeconds', 'businessSeconds')
MAX_WINDOWS = 20000
MAX_ACCESSES = 200000


def cooling_summary(db, account, gateway):
    """卡片只读最近的实际再访问，未访问的等待不生成结果或间隔。"""
    current, visits = None, []
    rows = db.execute('SELECT recovered,data FROM gp_cooling WHERE account=? AND gateway=? ORDER BY id DESC', (account,gateway))
    for index,row in enumerate(rows):
        steps = json.loads(row['data'])['steps']
        if index == 0 and row['recovered'] is None:
            current = steps[-1]
        for step in reversed(steps):
            if step['probeAt'] is not None:
                visits.append({**step,
                    'idleSeconds':max(0,step['probeAt']-step['startedAt']) if step['startedAt'] is not None else None,
                    'afterDueSeconds':max(0,step['probeAt']-step['dueAt'])})
                if len(visits) == 6:
                    return dict(current=current,visits=visits[:5],hasMore=True)
    return dict(current=current,visits=visits,hasMore=False)


def bounded(cursor, maximum):
    rows = cursor.fetchmany(maximum+1)
    if len(rows) > maximum:
        raise ValueError('统计范围记录较多，请缩短时间或选择账号后查询')
    return rows


def cooling_analysis(db, account, gateway, lower, upper):
    """按实际再访问起点归属范围；失败、待结果及缺失间隔不冒充恢复判定。"""
    counts = dict(recovered=0,unrecovered=0,failed=0,pending=0)
    bins = [dict(label=label,**counts) for label in ('不足 2h','2–4h','4–8h','8–16h','16h 及以上')]
    where,args = ["json_extract(s.value,'$.probeAt')>=?","json_extract(s.value,'$.probeAt')<?"],[lower,upper]
    for column,value in [('account',account),('gateway',gateway)]:
        if value:
            where.append('c.'+column+'=?');args.append(value)
    rows = bounded(db.execute("SELECT c.account,c.gateway,s.value FROM gp_cooling c,json_each(c.data,'$.steps') s WHERE "+' AND '.join(where),args),MAX_ACCESSES)
    pairs, recovered_intervals, missing = set(), [], 0
    for row in rows:
        step = json.loads(row['value'])
        pairs.add((row['account'],row['gateway']))
        result = step['result'] if step['resultAt'] is not None and step['resultAt'] <= upper else None
        key = {'21':'recovered','29':'unrecovered','failed':'failed'}.get(result,'pending')
        counts[key] += 1
        if step['startedAt'] is None or step['startedAt'] > step['probeAt']:
            missing += 1
            continue
        idle = step['probeAt']-step['startedAt']
        index = sum(idle >= boundary for boundary in (7200,14400,28800,57600))
        bins[index][key] += 1
        if key == 'recovered':
            recovered_intervals.append(idle)
    return dict(**counts,total=len(rows),gateways=len(pairs),missingInterval=missing,bins=bins,
                recoveredMedianSeconds=median(recovered_intervals) if recovered_intervals else None)


def union_seconds(intervals):
    total, previous = 0, -math.inf
    for start, end in sorted(intervals):
        total += max(0, end-max(start,previous))
        previous = max(previous,end)
    return total


def clip(start, end, lower, upper):
    if start is None or end is None:
        return None
    return max(0,min(upper,end)-max(lower,start))


def aggregate(rows):
    result = {key:sum(row[key] or 0 for row in rows) for key in METRICS}
    result['missing'] = {key:sum(row[key] is None and (key != 'windowSeconds' or row['verifiedAt'] is not None) for row in rows) for key in METRICS}
    result['requests'] = sum(row['requests'] for row in rows)
    result['records'] = len(rows)
    result['incomplete'] = sum(row['incomplete'] for row in rows)
    return result


def window_row(db_row, accesses, segments, lower, upper, now):
    item = json.loads(db_row['data'])
    def intervals(rows):
        return [(max(lower,a['started']),min(upper,a['ended'] if a['ended'] is not None else now))
                for a in rows if a['started'] is not None and not a['missing']
                and a['started'] < upper and (a['ended'] if a['ended'] is not None else now) >= lower]
    for a in accesses:
        a['missing'] = bool(a['incomplete'] or a['started'] is None or a['ended'] is None and (a['expires'] is None or a['expires'] <= now))
    # 确定在范围外的缺失访问不污染本次计时；结束未知仍保守保留。
    accesses = [a for a in accesses if (a['started'] if a['started'] is not None else a['observed']) < upper
                and (a['ended'] is None or a['ended'] >= lower)]
    business = [a for a in accesses if a['kind'] == 'business']
    legacy = [a for a in accesses if a['kind'] == 'legacy']
    active_intervals = intervals(business)
    known_business = union_seconds(active_intervals)
    missing_business = any(a['missing'] for a in business) or bool(legacy)
    preparation_end = item['probeEndedAt']
    preparation_start = item.get('preparationMeasuredAt',item['preparationStartedAt'])
    prep = clip(preparation_start,preparation_end if preparation_end is not None else now if item['state'] == 'preparing' else None,lower,upper)
    projected = {**item,
        'preparationSeconds':prep,
        'windowSeconds':clip(item['firstContactAt'],item['retiredAt'] if item['retiredAt'] is not None else now,lower,upper) if item['verifiedAt'] is not None else None,
        'businessSeconds':None if missing_business else known_business,
        'knownBusinessSeconds':known_business,
        'requestSeconds':sum(b-a for a,b in active_intervals),
        'ticketSeconds':sum(b-a for a,b in intervals([a for a in accesses if a['kind'] == 'ticket'])),
        'requests':sum(lower <= (a['started'] if a['started'] is not None else a['observed']) < upper for a in business),
        'incomplete':item['incomplete'] or any(a['missing'] for a in accesses),
        'partial':item['preparationStartedAt'] < lower or (item['drainedAt'] if item['drainedAt'] is not None else now) > upper,
        'segments':[]}
    for segment in segments:
        visits = [a for a in business if a['segment'] == segment['id']]
        interval = intervals(visits)
        if segment['started'] >= upper or (segment['last_end'] if segment['closed'] is not None and segment['last_end'] is not None else now) < lower:
            continue
        projected['segments'].append(dict(id=segment['id'],startedAt=segment['started'],endedAt=segment['last_end'],closedAt=segment['closed'],reason=segment['reason'],
                                          businessSeconds=union_seconds(interval),requestSeconds=sum(b-a for a,b in interval),
                                          requests=sum(lower <= a['started'] < upper for a in visits),incomplete=any(a['missing'] for a in visits)))
    return projected


def read(db, now, query):
    if not isinstance(query,dict):
        raise ValueError('统计参数必须为对象')
    meta = db.execute('SELECT * FROM gp_history_meta WHERE id=1').fetchone()
    lower, upper = query.get('from'), query.get('to')
    lower = meta['started'] if lower is None else lower
    upper = now if upper is None else upper
    if any(isinstance(v,bool) or not isinstance(v,(float,int)) or not math.isfinite(v) for v in (lower,upper)) or not 0 <= lower <= upper <= now+60:
        raise ValueError('统计时间范围无效')
    upper = min(upper,now)
    page, size = query.get('page',1),query.get('pageSize',20)
    if isinstance(page,bool) or not isinstance(page,int) or not 1 <= page <= 100000 or isinstance(size,bool) or not isinstance(size,int) or not 1 <= size <= 100:
        raise ValueError('分页参数无效')
    view = query.get('view','records')
    if view not in {'records','summary','cooling'}:
        raise ValueError('统计视图无效')
    account,gateway,batch,reason = (query.get(key) or '' for key in ('accountId','gateway','batchId','reason'))
    if any(not isinstance(v,str) or len(v)>160 for v in (account,gateway,batch,reason)):
        raise ValueError('筛选参数无效')
    where,args = ['w.started<=?','(w.ended IS NULL OR w.ended>=?)'],[upper,lower]
    for column,value in [('account',account),('gateway',gateway),('batch',batch)]:
        if value:
            where.append('w.'+column+'=?');args.append(value)
    if reason:
        where.append("json_extract(w.data,'$.reason')=?");args.append(reason)
    condition = ' AND '.join(where)
    windows = bounded(db.execute('SELECT w.* FROM gp_windows w WHERE '+condition+' ORDER BY w.started DESC,w.id DESC',args),MAX_WINDOWS) if view != 'cooling' else []
    accesses = defaultdict(list)
    for row in bounded(db.execute('SELECT a.*,l.expires FROM gp_accesses a JOIN gp_windows w ON w.id=a.generation LEFT JOIN leases l ON l.id=a.lease WHERE '+condition,args),MAX_ACCESSES) if windows else []:
        accesses[row['generation']].append(dict(row))
    segments = defaultdict(list)
    for row in bounded(db.execute('SELECT s.* FROM gp_segments s JOIN gp_windows w ON w.id=s.generation WHERE '+condition+' ORDER BY s.started',args),MAX_ACCESSES) if windows else []:
        segments[row['generation']].append(dict(row))
    records = [window_row(w,accesses[w['id']],segments[w['id']],lower,upper,now) for w in windows]
    summary = aggregate(records)
    # 未完整记录仍展示已知业务区间，比例的分母始终明确为已知时长。
    summary['businessSeconds'] = sum(r['knownBusinessSeconds'] for r in records)
    by_gateway,by_batch = defaultdict(list),defaultdict(list)
    for row in records:
        by_gateway[row['gateway']].append(row)
        by_batch[row['batchId']].append(row)
    gateways = []
    for name,items in by_gateway.items():
        total = aggregate(items);total['businessSeconds'] = sum(r['knownBusinessSeconds'] for r in items)
        gateways.append({'gateway':name,**total})
    batches = []
    for row in bounded(db.execute('SELECT b.* FROM gp_batches b WHERE EXISTS (SELECT 1 FROM gp_windows w WHERE w.batch=b.id AND '+condition+') ORDER BY b.started DESC,b.number DESC',args),MAX_WINDOWS) if view == 'summary' else []:
        entries = by_batch.get(row['id'])
        if not entries:
            continue
        candidates = json.loads(row['candidates'])
        processed = {key:value for key,value in json.loads(row['processed']).items() if key in candidates}
        totals = aggregate(entries);totals['businessSeconds'] = sum(r['knownBusinessSeconds'] for r in entries)
        batches.append(dict(id=row['id'],accountId=row['account'],number=row['number'],startedAt=row['started'],endedAt=row['ended'],status=row['status'],reason=row['reason'],
                            candidates=candidates,processed=processed,served=sum(v=='served' for v in processed.values()),
                            partial=row['started'] < lower or (row['ended'] if row['ended'] is not None else now) > upper or any(r['partial'] for r in entries) or bool(gateway or reason),**totals))
    cooling_where,cooling_args = ['started<=?','(recovered IS NULL OR recovered>=?)'],[upper,lower]
    for column,value in [('account',account),('gateway',gateway)]:
        if value:
            cooling_where.append(column+'=?');cooling_args.append(value)
    cycles = []
    for row in bounded(db.execute('SELECT * FROM gp_cooling WHERE '+' AND '.join(cooling_where)+' ORDER BY started DESC,id DESC',cooling_args),MAX_WINDOWS) if view == 'cooling' else []:
        data = json.loads(row['data']);step=data['steps'][-1]
        state = 'recovered' if row['recovered'] is not None else 'probing' if step['probeAt'] is not None and step['resultAt'] is None else 'due' if step['dueAt'] <= now else 'cooling'
        cycles.append(dict(id=row['id'],accountId=row['account'],gateway=row['gateway'],startedAt=row['started'],recoveredAt=row['recovered'],state=state,
                           incomplete=any(s['startedAt'] is None for s in data['steps']),**data))
    status = query.get('status') or ''
    if status:
        if status not in {'recovered','probing','due','cooling','incomplete'}:
            raise ValueError('恢复状态无效')
        cycles = [c for c in cycles if c['incomplete']] if status=='incomplete' else [c for c in cycles if c['state']==status]
    current = cycles if view=='cooling' else batches if view=='summary' else records
    count = len(current);pages=max(1,math.ceil(count/size));page=min(page,pages)
    selected = current[(page-1)*size:page*size]
    if view == 'records':
        # 使用租约代次精确关联，旧探测/排空回报不能误归当前网关的新轮转。
        for item in selected:
            condition = "json_extract(data,'$.generation')=?"
            item['observationCount'] = db.execute('SELECT COUNT(*) FROM observations WHERE '+condition,(item['id'],)).fetchone()[0]
            item['observations'] = [{**json.loads(r['data']),**{k:r[k] for k in ('id','at','account','gateway')}}
                                    for r in db.execute('SELECT * FROM observations WHERE '+condition+' ORDER BY id DESC LIMIT 100',(item['id'],))]
    accounts = [r['account'] for r in db.execute('SELECT DISTINCT account FROM gp_windows UNION SELECT DISTINCT account FROM gp_cooling ORDER BY account')]
    gateway_options = [r['gateway'] for r in db.execute('SELECT DISTINCT gateway FROM gp_windows'+(' WHERE account=?' if account else '')+' ORDER BY gateway',[account] if account else [])]
    batch_options = [{'id':r['id'],'accountId':r['account'],'number':r['number']} for r in db.execute('SELECT id,account,number FROM gp_batches'+(' WHERE account=?' if account else '')+' ORDER BY started DESC LIMIT 1000',[account] if account else [])]
    return dict(version=2,recordedSince=meta['started'],collectionGaps=meta['gaps'],**{'from':lower,'to':upper},
                page=dict(number=page,size=size,total=count,pages=pages),summary=summary,gateways=gateways,
                coolingAnalysis=cooling_analysis(db,account,gateway,lower,upper) if view=='summary' else None,
                records=selected if view=='records' else [],batches=selected if view=='summary' else [],recoveries=selected if view=='cooling' else [],
                options=dict(accounts=accounts,gateways=gateway_options,batches=batch_options))

"""Loopback-only UI fixtures layered over the real isolated plugin stage.

No credentials or upstream calls are used for demo accounts. This launcher is not
included in the production platform archive.
"""
import json
import sys
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'services/plugin_host'))
from server import Platform, BoundedServer, Handler

BASE = 'http://127.0.0.1:18335'
NOW = datetime.now(timezone.utc)
def at(days=0, hours=0):
    return (NOW + timedelta(days=days, hours=hours)).isoformat()
def response(value, code=200):
    return code, json.dumps({'code': code, 'message': '演示数据' if code == 200 else '演示账号不执行真实操作', 'data': value}, ensure_ascii=False).encode(), None

def account(plan, days, weight, used):
    usage = {'windowLabelDisplay':'演示用量', 'models':[], 'costs':[{'currency':'USD','estimatedAmount':'12.34','estimatedAmountDisplay':'$12.34'}], 'costEstimateStatus':'known'}
    for key in ('requestCount','inputTokens','outputTokens','cachedTokens','reasoningTokens','imageInputTokens','imageOutputTokens','imageRequestCount','imageRequestFailedCount','totalTokens','createdTokens','readTokens','knownCostCount','partialCostCount','unknownCostCount'):
        usage[key] = 1200 if 'Tokens' in key else 12
        usage[key+'Display'] = '1.2K' if 'Tokens' in key else '12'
    usage.update(lastUsedAt=at(hours=-1), lastUsedAtDisplay='1 小时前')
    windows=[]
    for key, label, seconds, percent, reset in [('shortTerm','5 小时',18000,used,at(hours=3)),('weekly','每周',604800,used+15,at(days=4))]:
        windows.append(dict(key=key,group='shortTerm',limitId='codex',limitName='Codex',role='primary' if key=='shortTerm' else 'secondary',windowSeconds=seconds,labelDisplay=label,windowLabelDisplay=label,usedPercent=percent,usedPercentDisplay=f'{percent}%',limitReached=False,resetAt=reset,resetAtDisplay=reset))
    return dict(id='preview-'+plan,name='【演示】'+plan.title()+' · 仅界面预览',notes='模拟账号，没有登录凭据，不调用上游。',provider='openai',resourceRef='preview-'+plan,email=plan+'@example.invalid',accountId='preview-'+plan,userId='preview-user',label=None,planType=plan,planTypeDisplay=plan.title(),authenticationKind='oauth',hasRefreshToken=False,status='normal',errorReason=None,errorMessage=None,enabled=True,concurrencyLimit=2,weight=weight,effectiveWeight=weight,lifecycle=dict(expiryPriority=False,archived=False,archiveReason=None,archivedAt=None,subscriptionExpiresAt=at(days=days),subscriptionObservedAt=at()),modelAccess=dict(mode='all',models=[]),accessTokenExpiresAt=at(days=7),accessTokenExpiresAtDisplay=at(days=7),refreshTokenExpiresAt=None,nextRefreshAt=None,addedAt=at(days=-7),addedAtDisplay=at(days=-7),updatedAt=at(),updatedAtDisplay=at(),outboundProxyEndpoint=None,quota=dict(credits={'balance':'1000','unlimited':False,'usdEquivalent':'10.00'},refreshedAtDisplay='演示数据',limitReached=False,rateLimitedUntil=None,rateLimitReason=None,recoveryProbeRequired=False,windows=windows),usage=usage,groups=[])

class PreviewPlatform(Platform):
    def __init__(self):
        super().__init__(ROOT/'.build/plugin-platform',18336,'ApiHubBuild-20260928')
        self.demo_accounts=[account('plus',3,50,28),account('pro',21,100,12)]
        self.demo_jobs={}

    def forward(self, method, path, cookie='', data=None, origin=None, bearer=None):
        parsed=urlsplit(path)
        values={key:value[0] for key,value in parse_qs(parsed.query).items()}
        values.update(data or {})
        if parsed.path == '/api/admin/client-keys' and method == 'GET':
            status, body, header = super().forward(method, path, cookie, data, origin, bearer)
            if status != 200: return status, body, header
            payload = json.loads(body)
            demo = dict(id='preview-key', name='【演示】CC Switch 导入预览', label='无效演示密钥，不可调用模型', prefix='sk-preview', enabled=True,
                        openaiClientProfileOverride=None, xaiClientProfileOverride=None, maxConcurrency=2, requestsPerMinute=0,
                        dailyLimitUsd='0', weeklyLimitUsd='0', dailyUsedUsd='0', weeklyUsedUsd='0', dailyResetsAt=None,
                        weeklyResetsAt=None, createdAt=at(), updatedAt=at(), lastUsedAt=None, routingScope='account', groups=[], providerKinds=['openai'])
            if not values.get('cursor') and (not values.get('search') or values['search'] in demo['name']):
                payload['data']['items'].insert(0, demo)
                payload['data']['total'] += 1
            return status, json.dumps(payload, ensure_ascii=False).encode(), header
        if parsed.path == '/api/admin/client-keys/bindings' and 'preview-key' in values.get('ids', []):
            ids = [key for key in values['ids'] if key != 'preview-key']
            result = {'configRevision': 1, 'items': []}
            if ids:
                status, body, header = super().forward(method, path, cookie, {'ids': ids}, origin, bearer)
                if status != 200: return status, body, header
                result = json.loads(body)['data']
            result['items'].append({'id':'preview-key','accountId':'preview-plus','routing':None,'excelBridgeEnabled':False})
            return response(result)
        if parsed.path == '/api/admin/client-keys/reveal' and values.get('id') == 'preview-key':
            return response({'id':'preview-key','plaintextKey':'sk-preview-invalid-not-a-real-key'})
        if parsed.path == '/v1/models' and bearer == 'sk-preview-invalid-not-a-real-key':
            return 200, json.dumps({'data':[{'id':'gpt-6-astra'},{'id':'gpt-6.1-sol'}]}).encode(), None
        selected=next((item for item in self.demo_accounts if item['id']==values.get('accountId')),None)
        if parsed.path=='/api/admin/accounts' and method=='GET':
            status,body,header=super().forward(method,path,cookie,data,origin,bearer)
            if status!=200:return status,body,header
            payload=json.loads(body)
            result=payload['data']
            demos=[] if values.get('archived')=='true' or values.get('groupId') else self.demo_accounts
            demos=[item for item in demos if (not values.get('search') or values['search'].lower() in (item['name']+item['email']).lower()) and (not values.get('provider') or values['provider']==item['provider']) and (not values.get('status') or values['status']==item['status'])]
            if int(values.get('page','1'))==1: result['items']=demos+result['items']
            result['page']['total']+=len(demos)
            result['page']['totalPages']=max(1,result['page']['totalPages'])
            result['summary']['total']+=len(self.demo_accounts)
            result['summary']['normal']+=len(self.demo_accounts)
            return status,json.dumps(payload,ensure_ascii=False).encode(),header
        if selected:
            endpoint=parsed.path.rsplit('/',1)[-1]
            if endpoint=='personal-info':return response({'profile':None,'profileError':'演示账号，无真实个人资料','subscription':{'startsAt':at(days=-7),'expiresAt':selected['lifecycle']['subscriptionExpiresAt'],'willRenew':False,'billingPeriod':'monthly','billingCurrency':'USD','observedAt':at()}})
            if endpoint in ('models','refresh') and '/models' in parsed.path:return response({'models':[{'id':model,'name':model} for model in ['gpt-6-astra','gpt-6.1-sol','gpt-6-sol']]})
            if endpoint in ('refresh','recover'):return response({'account':selected})
            if endpoint=='reset-credits':return response({'availableCount':0,'credits':[]})
            return response({'message':'演示账号仅供查看和模拟诊断，不执行真实账号操作。'},400)
        # Any fixture mutation must stay out of the real gateway.
        if any('preview-' in str(values.get(key,'')) for key in ('id','ids','accountIds','accountId')):
            return response({'message':'演示账号不执行真实账号操作。'},400)
        return super().forward(method,path,cookie,data,origin,bearer)

    def call(self, plugin_id, version, name, data, cookie, role, origin, draining=False):
        is_demo=plugin_id=='account-diagnostics' and (str(data.get('accountId','')).startswith('preview-') or str(data.get('id','')).startswith('preview-task-'))
        if not is_demo:return super().call(plugin_id,version,name,data,cookie,role,origin,draining)
        if role!='admin' or not self.store.state(plugin_id)['enabled'] or name not in self.store.manifest(plugin_id,version)['capabilities']:
            raise PermissionError('诊断预览未授权')
        if name=='tasks.start':
            quick=data.get('mode')=='quick'
            task_id='preview-task-'+uuid.uuid4().hex
            result={'status':200,'durationMs':328,'at':at(),'stateReturned':False,'classification':'21' if quick else None,'bootstrapPerformed':quick,'reason':'【界面演示】这是模拟检测结果，不代表真实账号能力。'}
            if not quick:
                result['text']='【界面演示 · 非模型输出】\n\n已收到测试内容：\n'+str(data.get('prompt','hi'))+'\n\n'+('\n'.join(f'{i}. 演示回答：用于查看正文排版、字体、换行与滚动。结果增长时，底部测试按钮仍应保持可见。' for i in range(1,21)))
            task={'id':task_id,'status':'succeeded','result':result,'accountId':data['accountId']}
            self.demo_jobs[task_id]=(cookie,task)
            return response(task)
        if name=='diagnostics.history':return response([task for owner,task in self.demo_jobs.values() if owner==cookie and task['accountId']==data['accountId']])
        if name in ('tasks.status','tasks.cancel'):
            owner,task=self.demo_jobs[data['id']]
            if owner!=cookie:raise PermissionError('任务未授权')
            return response(task)
        return super().call(plugin_id,version,name,data,cookie,role,origin,draining)

if __name__=='__main__':
    host=BoundedServer(('127.0.0.1',18335),Handler)
    host.platform=PreviewPlatform()
    host.origin=BASE
    host.assets=ROOT/'frontend/dist'
    host.local_access=ROOT/'.build/plugin-stage/access.json'
    print('Local preview with labeled demo accounts: '+BASE,flush=True)
    host.serve_forever()

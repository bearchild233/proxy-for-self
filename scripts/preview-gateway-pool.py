"""真实主壳、插件包和统计 SQLite 的回环预览；全部账号和访问事件为模拟数据。

先构建 frontend 主壳以及 gateway-pool、keys 两个插件。
此入口不接 Rust 网关、不连接上游，每次启动使用独立临时数据库。
"""
import argparse
import json
from pathlib import Path
import sys
import tempfile
import time
import uuid
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'services/plugin_host'))
from server import BoundedServer, Handler, Platform


def response(value, status=200):
    return status, json.dumps({'code':status,'message':'模拟数据预览','data':value}, ensure_ascii=False).encode(), None


def seed(pool):
    now = time.time() - 20 * 3600
    pool.clock = lambda: now
    with pool.db() as db:
        db.execute('UPDATE gp_history_meta SET started=?', (now,))
    gateways = ['unified-142','unified-143','unified-144','unified-145']
    pool.configure({'gateways':gateways,'groupIds':['preview-group'],'initialRestSeconds':7200,
                    'dynamicGroups':True,'normalGroupId':'preview-normal-group','degradedGroupId':'preview-group'})

    def call(action, account, **data):
        return pool.runtime({'action':action,'accountId':account,**data})

    def access(account, lease, purpose, seconds, action, **extra):
        nonlocal now
        key, started = uuid.uuid4().hex, now
        call('sent',account,leaseId=lease,timingVersion=2,accessId=key,purpose=purpose)
        now += seconds
        call(action,account,leaseId=lease,access={'id':key,'startedAt':started,'elapsedMs':seconds*1000,'ended':True,'outcome':'completed'},**extra)

    for wave in range(3):
        for account in ('preview-alpha','preview-beta'):
            for index,gateway in enumerate(gateways):
                claim = call('claim',account,demand=True,catalog=[{'gateway':name,'validForS':900} for name in gateways])
                if not claim.get('claimed'):
                    continue
                lease, pair, start = claim['leaseId'],claim['pair'],now
                gateway = pair['gateway']
                degraded = wave == 0 and index == 2 or wave == 1 and index == 3
                for phase in ('bootstrap','replay'):
                    access(account,lease,phase,1.2+index/3,'probeAttempt',phase=phase,retry=0,status=200,stateReturned=phase=='bootstrap' or degraded)
                call('probeResult',account,leaseId=lease,status=200,stateReturned=degraded,paired=True,gatewayMatches=True,preparationStartedAt=start,durationMs=(now-start)*1000)
                call('finish',account,leaseId=lease)
                if not degraded:
                    for request in range(3):
                        lease = call('reserve',account,gateway=gateway,generation=pair['generation'],deadline=now+900)['leaseId']
                        if request == 0:
                            access(account,lease,'ticket',0.8,'ticketResult',model='gpt-6-astra',status=200)
                        access(account,lease,'business',12+index*8+wave*9,'finish')
                        now += 8
                    lease = call('reserve',account,gateway=gateway,generation=pair['generation'],deadline=now+900)['leaseId']
                    call('invalidate',account,leaseId=lease)
                    call('finish',account,leaseId=lease)
                now += 20
        now += (wave+1)*2*3600
    # 已移出分组的账号仍有历史记录和一个服务中的组合：页面不应再显示它。
    claim = call('claim','preview-normal',demand=True,catalog=[{'gateway':name,'validForS':900} for name in gateways])
    call('sent','preview-normal',leaseId=claim['leaseId'])
    call('probeResult','preview-normal',leaseId=claim['leaseId'],status=200,stateReturned=False,paired=True,gatewayMatches=True)
    call('finish','preview-normal',leaseId=claim['leaseId'])
    # 模拟百个候选的展示密度，轮转历史仍来自上述真实状态机事件。
    with pool.db() as db:
        for account in ('preview-alpha','preview-beta'):
            pool.catalog(db,account,[{'gateway':f'unified-{number}','validForS':900} for number in range(142,242)],now)
    # 动态分组记录：确认降智移入、持续满血恢复移回、一次写入失败、一次处理中。
    for offset,account,direction,status in [(7200,'preview-alpha','degraded','moved'),(5400,'preview-beta','degraded','moved'),(3000,'preview-alpha','recovered','moved'),(900,'preview-beta','recovered','failed'),(60,'preview-normal','degraded',None)]:
        pool.clock = lambda offset=offset: now - offset
        event = pool.runtime({'action':'regroup','accountId':account,'direction':direction,'model':'gpt-6.1-sol'})['move']['eventId']
        if status:
            pool.clock = lambda offset=offset: now - offset + 2
            pool.runtime({'action':'regroupResult','accountId':account,'eventId':event,'status':status})
    pool.clock = time.time


class PreviewPlatform(Platform):
    def __init__(self, root):
        super().__init__(root, 0)
        self.install('gateway-pool')
        self.install('keys')
        self.store.action('gateway-pool','enable')
        # 批量生成模拟记录时保持 WAL 连接，避免 Windows 反复关闭/重建辅助文件。
        with self.gateway_pool.db(readonly=True) as snapshot:
            snapshot.execute('SELECT COUNT(*) FROM pairs').fetchone()
            seed(self.gateway_pool)

    def forward(self, method, path, cookie='', data=None, origin=None, bearer=None):
        path = urlsplit(path).path
        if method == 'GET' and path == '/api/auth/status':
            return response({'authenticated':True,'session':{'role':'admin','expiresAt':'2099-01-01T00:00:00Z'}})
        if method == 'GET' and path == '/api/admin/accounts':
            items = [{'id':'preview-'+key,'name':'【模拟】账号 '+name,'email':key+'@example.invalid','provider':'openai','authenticationKind':'oauth','planTypeDisplay':'Plus','enabled':True,'groups':[{'id':group,'enabled':True}]} for key,name,group in [('alpha','A','preview-group'),('beta','B','preview-group'),('normal','普通','preview-normal-group')]]
            return response({'items':items,'page':{'totalPages':1}})
        if method == 'GET' and path == '/api/admin/account-groups':
            return response({'items':[{'id':id,'name':name,'enabled':True,'color':'#547DFFFF','description':'模拟数据','accountCount':count} for id,name,count in [('preview-group','【模拟】降智组',2),('preview-normal-group','【模拟】普通组',1)]],'page':{'totalPages':1}})
        if method == 'GET' and path == '/api/admin/client-keys':
            return response({'items':[],'total':0,'nextCursor':None})
        if method == 'GET' and path == '/api/admin/system/version':
            return response({'version':'本地模拟预览','gitSha':'','buildTime':'','deploymentMode':'source','deploymentModeLabel':'模拟数据','updateChannel':'','latestVersion':'','hasUpdate':False,'updateCached':False,'updateWarning':None})
        return response(None,403)

    def call(self, plugin_id, version, name, data, cookie, role, origin, draining=False):
        if name == 'gateway-pool.configure':
            raise PermissionError('此预览只供查看模拟记录')
        status,body,session = super().call(plugin_id,version,name,data,cookie,role,origin,draining)
        if name == 'gateway-pool.read' and status == 200:
            body = json.dumps({**json.loads(body),'preview':True},ensure_ascii=False).encode()
        return status,body,session


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port',type=int,default=18335)
    args = parser.parse_args()
    for path in (ROOT/'frontend/dist/index.html',ROOT/'.build/plugins/gateway-pool/package/plugin.json',ROOT/'.build/plugins/keys/package/plugin.json'):
        if not path.is_file():
            parser.error('请先构建主壳和网关池插件')
    preview_root = ROOT/'.build/preview'
    preview_root.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='gateway-pool-',dir=preview_root) as directory:
        server = BoundedServer(('127.0.0.1',args.port),Handler)
        server.platform = PreviewPlatform(directory)
        server.origin = f'http://127.0.0.1:{args.port}'
        server.assets = ROOT/'frontend/dist'
        server.local_access = None
        print(server.origin+'/extensions/gateway-pool — 模拟数据，无真实上游连接',flush=True)
        try:
            server.serve_forever()
        finally:
            server.server_close()


if __name__ == '__main__':
    main()

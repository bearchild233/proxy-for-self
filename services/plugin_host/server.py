"""Versioned plugin control plane. Inference traffic goes directly to the gateway."""
import argparse
from contextvars import ContextVar
from ipaddress import ip_address
import base64
import gzip
import hashlib
from http.client import HTTPConnection, HTTPException
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import mimetypes
import re
from pathlib import Path
import sys
import threading
import time
sys.path.insert(0, str(Path(__file__).resolve().parent))
from jobs import Jobs
from lifecycle import Lifecycle
import tarfile
import tempfile
from urllib.parse import urlencode, urlsplit

REPO = Path(__file__).resolve().parents[2]
CLIENT_IP = ContextVar('plugin_client_ip', default='127.0.0.1')
sys.path.insert(0, str(REPO / 'services/updater'))
from static_plugins import StaticPluginStore, PluginVersionChangedError


def package_directory(source, archive):
    with archive.open('wb') as output, gzip.GzipFile(fileobj=output, mode='wb', mtime=0, filename='') as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as package:
            for file in sorted(source.rglob('*')):
                if file.is_file() and not file.is_symlink():
                    info = package.gettarinfo(file, arcname=file.relative_to(source).as_posix())
                    info.mtime = info.uid = info.gid = 0
                    info.uname = info.gname = ''
                    info.mode = 0o644
                    with file.open('rb') as content:
                        package.addfile(info, content)


class Platform:
    def __init__(self, root, gateway_port, local_excel=None, updater_socket=None):
        self.root = Path(root).resolve()
        self.gateway_port = gateway_port
        self.policies = json.loads((REPO / 'packages/plugin-sdk/policies.json').read_text(encoding='utf-8'))
        registry=self.root/'registry.json'
        self.root.mkdir(parents=True,exist_ok=True)
        if registry.exists():
            self.policies.update(json.loads(registry.read_text(encoding='utf-8')))
        self.operations = json.loads((REPO / 'packages/plugin-sdk/operations.json').read_text(encoding='utf-8'))
        self.store = StaticPluginStore(self.root / 'plugins', self.policies)
        self.jobs = Jobs(self)
        self.lifecycle = Lifecycle(self,local_excel,updater_socket)
        self.data_lock = threading.RLock()

    def register(self, registration):
        plugin_id=registration.get('id')
        caps=registration.get('capabilities',[])
        if not isinstance(plugin_id,str) or not re.fullmatch(r'[a-z][a-z0-9-]{0,63}',plugin_id) or plugin_id in self.policies:
            raise ValueError('插件标识无效或已注册')
        if not isinstance(caps,list) or not all(isinstance(cap,str) for cap in caps) or not set(caps)<=(set(self.operations)|{'tasks.start','tasks.status','tasks.cancel'}):
            raise ValueError('只能申请平台已提供的能力')
        if any(not isinstance(registration.get(key),str) or not 0<len(registration[key])<=2000 for key in ('name','description')):
            raise ValueError('插件介绍无效')
        policy={'name':registration['name'],'description':registration['description'],'category':'extensions','required':False,'capabilities':caps,'worker':registration.get('worker') is True}
        with self.store.mutation():
            path=self.root/'registry.json'
            values=json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
            values[plugin_id]=policy
            temporary=path.with_suffix('.tmp');temporary.write_text(json.dumps(values,ensure_ascii=False),encoding='utf-8');temporary.replace(path)
            self.policies[plugin_id]=policy
        return policy

    def forward(self, method, path, cookie='', data=None, origin=None, bearer=None):
        connection = HTTPConnection('127.0.0.1', self.gateway_port, timeout=75)
        try:
            headers = {'Cookie': cookie, 'Content-Type': 'application/json', 'X-Real-IP': CLIENT_IP.get()}
            if origin:
                headers['Origin'] = origin
            if bearer:
                headers['Authorization'] = 'Bearer ' + bearer
            connection.request(method, path, None if data is None else json.dumps(data).encode(), headers)
            response = connection.getresponse()
            body = response.read(8 * 1024 * 1024 + 1)
            if len(body) > 8 * 1024 * 1024:
                raise ValueError('响应超过限制')
            return response.status, body, response.getheader('Set-Cookie')
        finally:
            connection.close()

    def role(self, cookie):
        status, body, _ = self.forward('GET', '/api/auth/status', cookie)
        session = json.loads(body).get('data', {}).get('session') if status == 200 else None
        if not session or session.get('role') not in {'admin', 'key'}:
            raise PermissionError('请先登录')
        return session['role']

    def loading_preferences(self):
        path = self.root / 'loading.json'
        return json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}

    def set_loading(self, plugin_id, lazy):
        if plugin_id not in self.policies or not isinstance(lazy, bool):
            raise ValueError('加载设置无效')
        with self.store.mutation():
            values = self.loading_preferences()
            values[plugin_id] = lazy
            target = self.root / 'loading.json'
            temporary = target.with_suffix('.tmp')
            temporary.write_text(json.dumps(values), encoding='utf-8')
            temporary.replace(target)
        return {'lazyLoad': lazy}

    def catalog(self, role, management=False):
        loading = self.loading_preferences()
        result = []
        for plugin_id, policy in self.policies.items():
            state = self.store.state(plugin_id)
            if not management and not state['enabled']:
                continue
            manifest = self.store.manifest(plugin_id) if state['current'] else None
            pages = [{**{k: v for k, v in page.items() if k != 'entry'}, 'runtime': manifest.get('runtime', 'iframe')} for page in (manifest or {}).get('pages', []) if role in page['roles']]
            if not management and not pages:
                continue
            def release_info(version):
                if not version:
                    return None
                release = self.store.manifest(plugin_id, version)
                return {'version': release['version'], 'contentVersion': version,
                        'releasedAt': release.get('releasedAt'), 'notes': release.get('releaseNotes', [])}
            result.append({'id': plugin_id, **policy, 'pages': pages, 'lazyLoad': loading.get(plugin_id) is True,
                           **({'currentRelease': release_info(state['current']), 'previousRelease': release_info(state['previous'])} if management else {}),
                           'installed': bool(manifest), 'enabled': state['enabled'], 'bundled':(REPO/'.build/plugins'/plugin_id/'package/plugin.json').is_file(),
                           'version': manifest['version'] if manifest else None, 'contentVersion': state['current'],
                           'operation':self.lifecycle.operations.get(plugin_id), 'canRollback': bool(state['previous']) and plugin_id!='excel-bridge'})
        order_file = self.root / 'order.json'
        order = json.loads(order_file.read_text()) if order_file.exists() else []
        return sorted(result, key=lambda p: (p['required'], order.index(p['id']) if p['id'] in order else 999, p['id']))

    def install(self, plugin_id):
        with tempfile.TemporaryDirectory(dir=self.root, prefix='.package-') as directory:
            archive = Path(directory) / 'package.tar.gz'
            package_directory(REPO / '.build/plugins' / plugin_id / 'package', archive)
            return self.store.install(plugin_id, archive, hashlib.sha256(archive.read_bytes()).hexdigest())

    def call(self, plugin_id, version, name, data, cookie, role, origin, draining=False):
        if not draining and not self.store.state(plugin_id)['enabled']:
            raise PermissionError('插件已停用，请重新打开页面')
        manifest = self.store.manifest(plugin_id, version)
        if name not in manifest['capabilities']:
            raise PermissionError('插件没有此能力')
        if not isinstance(data, dict):
            raise ValueError('参数必须为对象')
        if name == 'pricing.schedule.status':
            if role != 'admin' or plugin_id != 'pricing':
                raise PermissionError('没有定价调度权限')
            path = self.root / 'pricing-runtime' / 'status.json'
            state = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {}
            visible = {key: state.get(key) for key in ('status','lastAttempt','lastSuccess','nextRun','checkedAt','error')}
            return 200, json.dumps(visible, ensure_ascii=False).encode(), None
        if name.startswith(('tasks.','templates.','diagnostics.')):
            if role != 'admin' or (not name.startswith('tasks.') and plugin_id != 'account-diagnostics'):
                raise PermissionError('没有诊断权限')
            if name == 'tasks.start':
                if plugin_id == 'account-diagnostics' and (not isinstance(data.get('accountId'),str) or data.get('mode') not in {'quick','text'} or len(str(data.get('prompt',''))) > 32768):
                    raise ValueError('诊断参数无效')
                result=self.jobs.start(plugin_id,version,data,cookie,role,origin)
            elif name in {'tasks.status','tasks.cancel'}:
                result=self.jobs.get(data.get('id'),cookie,name=='tasks.cancel')
            elif name == 'diagnostics.history':
                result=self.history(data.get('accountId'))
            else:
                result=self.templates(plugin_id,version,data if name=='templates.write' else None)
            return 200,json.dumps(result,ensure_ascii=False).encode(),None
        if name == 'accounts.avatar':
            if role != 'admin' or plugin_id != 'accounts':raise PermissionError('头像访问未授权')
            status,body,_=self.forward('GET','/api/admin/accounts/profile-avatar?'+urlencode({'accountId':data.get('accountId')}),cookie)
            mime='image/png' if body.startswith(b'\x89PNG') else 'image/jpeg' if body.startswith(b'\xff\xd8') else 'image/webp' if body.startswith(b'RIFF') else None
            if status!=200 or not mime or len(body)>3*1024*1024:raise ValueError('头像不可用')
            return 200,json.dumps({'url':'data:'+mime+';base64,'+base64.b64encode(body).decode()}).encode(),None
        if name == 'excel.status':
            if role != 'admin':
                return 200,json.dumps({'installed':True,'enabled':True}).encode(),None
            return 200,json.dumps(self.lifecycle.excel('status',cookie,origin)).encode(),None
        if name == 'client.models':
            secret=data.get('apiKey')
            if not isinstance(secret,str) or not 0<len(secret)<512:
                raise ValueError('密钥无效')
            status,body,_=self.forward('GET','/v1/models',bearer=secret)
            models=json.loads(body).get('data',[]) if status==200 else []
            return 200,json.dumps(sorted({item['id'] for item in models if isinstance(item,dict) and isinstance(item.get('id'),str)})).encode(),None
        if name not in self.operations:
            raise PermissionError('能力未注册')
        spec = self.operations[name]
        if role not in spec['roles']:
            raise PermissionError('当前身份没有此权限')
        if not isinstance(data, dict):
            raise ValueError('参数必须为对象')
        if name == 'display-order.save':
            scope = {'accounts':'accounts','groups':'groups','keys':'keys','proxy-management':'proxies'}.get(plugin_id)
            if not scope or data.get('scope') != scope:
                raise PermissionError('不能修改其他模块的顺序')
        path = spec['path']
        if spec['method'] == 'GET':
            path += '?' + urlencode({key: str(value).lower() if isinstance(value, bool) else value for key, value in data.items() if value is not None})
        return self.forward(spec['method'], path, cookie, data if spec['method'] == 'POST' else None, origin)

    def templates(self, plugin_id, version, value=None):
        path=self.root/'diagnostic-templates.json'
        with self.data_lock:
            if value is not None:
                items=value.get('items')
                if not isinstance(items,list) or not 1<=len(items)<=32 or any(not isinstance(item,dict) or set(item)!={'id','name','prompt'} or any(not isinstance(v,str) or not v for v in item.values()) or len(item['prompt'])>32768 or len(item['name'])>80 for item in items) or len({item['id'] for item in items})!=len(items):
                    raise ValueError('模板无效')
                temporary=path.with_suffix('.tmp');temporary.write_text(json.dumps(items,ensure_ascii=False),encoding='utf-8');temporary.replace(path)
            return json.loads(path.read_text(encoding='utf-8')) if path.exists() else json.loads((self.store.directory(plugin_id)/'releases'/version/'templates.json').read_text(encoding='utf-8'))

    def history(self, account_id=None, append=None):
        path=self.root/'diagnostic-history.json'
        with self.data_lock:
            rows=json.loads(path.read_text(encoding='utf-8')) if path.exists() else []
            if append is not None:
                rows=(rows+[append])[-200:]
                temporary=path.with_suffix('.tmp');temporary.write_text(json.dumps(rows,ensure_ascii=False),encoding='utf-8');temporary.replace(path)
            return [row for row in rows if row.get('accountId')==account_id][-30:]


class Handler(BaseHTTPRequestHandler):
    def reply(self, value, status=200, cookie=None, content_type='application/json; charset=utf-8'):
        body = value if isinstance(value, bytes) else json.dumps(value, ensure_ascii=False).encode()
        compressed = (status == 200 and self.path.startswith('/api/plugin-platform/page/')
                      and any(part.strip().split(';')[0] == 'gzip' and 'q=0' not in part for part in self.headers.get('Accept-Encoding', '').split(',')))
        if compressed:
            body = gzip.compress(body, compresslevel=4)
        self.send_response(status)
        if compressed:
            self.send_header('Content-Encoding', 'gzip')
            self.send_header('Vary', 'Accept-Encoding')
        for key, val in [('Content-Type',content_type),('Content-Length',str(len(body))),('Cache-Control','no-store'),('X-Content-Type-Options','nosniff')]:
            self.send_header(key, val)
        if cookie:
            self.send_header('Set-Cookie', cookie)
        self.end_headers()
        self.wfile.write(body)

    def dispatch(self):
        self.connection.settimeout(15)
        platform = self.server.platform
        origin = self.server.origin
        if self.headers.get('Host') != urlsplit(origin).netloc:
            raise PermissionError('请求主机无效')
        if self.command == 'POST' and self.headers.get('Origin') != origin:
            raise PermissionError('请求来源无效')
        path = urlsplit(self.path).path
        cookie = self.headers.get('Cookie', '')
        data = None
        if self.command == 'POST':
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size <= 24 * 1024 * 1024:
                raise ValueError('请求大小无效')
            data = json.loads(self.rfile.read(size))
            if not isinstance(data,dict):raise ValueError('请求必须为对象')
        if path == '/healthz':
            return self.reply({'ok': True, 'service': 'plugin-platform'})
        if path == '/api/plugin-platform/local-login' and self.command == 'POST' and self.server.local_access:
            access = json.loads(self.server.local_access.read_text())
            status, body, session = platform.forward('POST', '/api/auth/login', data={'mode':'admin', **access}, origin=origin)
            return self.reply(body, status, session)
        if path in {'/api/auth/status','/api/auth/login','/api/auth/logout','/api/auth/password','/api/admin/system/version','/api/admin/system/update/detail','/api/admin/system/update/status','/api/admin/system/update','/api/admin/system/restart'}:
            status, body, session = platform.forward(self.command, self.path, cookie, data, origin)
            return self.reply(body, status, session)
        if path.startswith('/api/plugin-platform/'):
            role = platform.role(cookie)
            tail = path.removeprefix('/api/plugin-platform/').split('/')
            if tail == ['catalog'] and self.command == 'GET':
                return self.reply(platform.catalog(role))
            if tail == ['manage'] and self.command == 'GET' and role == 'admin':
                return self.reply(platform.catalog(role, True))
            if len(tail) == 4 and tail[0] == 'page' and self.command == 'GET':
                return self.reply({'html': platform.store.page(tail[1], tail[2], tail[3], role),'lease':platform.store.lease(tail[1],tail[2])})
            if len(tail) == 4 and tail[0] == 'call' and self.command == 'POST':
                status, body, session = platform.call(*tail[1:], data, cookie, role, origin)
                return self.reply(body, status, session)
            if tail == ['lease'] and self.command == 'POST':
                return self.reply({'lease':platform.store.lease(data['id'],data['version'],data['lease'],data.get('release',False))})
            if tail == ['action'] and self.command == 'POST' and role == 'admin':
                plugin_id, action = data.get('id'), data.get('action')
                platform.store.directory(plugin_id)
                result = platform.lifecycle.begin(plugin_id,action,cookie,origin,data.get('expectedCurrent'),data.get('expectedPrevious'))
                return self.reply(result)
            if tail == ['loading'] and self.command == 'POST' and role == 'admin':
                return self.reply(platform.set_loading(data.get('id'), data.get('lazyLoad')))
            if tail == ['order'] and self.command == 'POST' and role == 'admin':
                ids = data.get('ids')
                if not isinstance(ids, list) or not all(isinstance(item,str) for item in ids) or len(ids) != len(set(ids)) or set(ids) != set(platform.policies):
                    raise ValueError('排序列表无效')
                flags = [platform.policies[item]['required'] for item in ids]
                if flags != sorted(flags):
                    raise ValueError('基础模块必须保留在后面')
                with platform.store.mutation():
                    temporary = platform.root / '.order.json'
                    temporary.write_text(json.dumps(ids))
                    temporary.replace(platform.root / 'order.json')
                return self.reply({'saved':True})
            if tail == ['register'] and self.command == 'POST' and role == 'admin':
                return self.reply(platform.register(data))
            if tail == ['upload'] and self.command == 'POST' and role == 'admin':
                raw = base64.b64decode(data['package'], validate=True)
                with tempfile.TemporaryDirectory(dir=platform.root, prefix='.upload-') as directory:
                    archive = Path(directory) / 'package.tar.gz'
                    archive.write_bytes(raw)
                    if hashlib.sha256(raw).hexdigest()!=data['sha256']:
                        raise ValueError('安装包校验失败')
                    with tarfile.open(archive,'r:gz') as package:
                        member=package.getmember('plugin.json')
                        if not member.isfile() or member.size>65536:raise ValueError('清单无效')
                        manifest=json.load(package.extractfile(member))
                    if not isinstance(manifest,dict):raise ValueError('清单必须为对象')
                    plugin_id=manifest.get('id')
                    if not isinstance(plugin_id,str):raise ValueError('插件标识无效')
                    if plugin_id=='excel-bridge' and platform.lifecycle.operations.get(plugin_id,{}).get('status')=='running':raise ValueError('Worker 操作仍在执行')
                    fresh=plugin_id not in platform.policies
                    if fresh:
                        slots={'settings','key-usage','key-editor','key-config','account-menu','account-detail','group-menu','key-menu','proxy-menu','usage-detail','dashboard'}
                        if not isinstance(manifest.get('pages'),list) or any(not isinstance(page,dict) or (page.get('route') is not None and (not isinstance(page['route'],str) or not page['route'].startswith('/extensions/'+str(plugin_id)+'/'))) or (not page.get('route') and page.get('slot') not in slots) for page in manifest['pages']):
                            raise ValueError('新增插件的页面必须使用独立扩展路径')
                        platform.register({**manifest,'worker':bool(manifest.get('worker'))})
                    try:
                        return self.reply(platform.store.install(plugin_id,archive,data['sha256']))
                    except Exception:
                        if fresh:
                            with platform.store.mutation():
                                registry=platform.root/'registry.json'
                                values=json.loads(registry.read_text(encoding='utf-8'));values.pop(plugin_id,None)
                                temporary=registry.with_suffix('.tmp');temporary.write_text(json.dumps(values,ensure_ascii=False),encoding='utf-8');temporary.replace(registry)
                                platform.policies.pop(plugin_id,None)
                        raise
            raise PermissionError('插件操作不可用')
        if path.startswith('/api/') or self.command != 'GET':
            return self.reply({'error':'接口不存在'},404)
        # 发布目录通过 symlink 原子切换；每个请求固定一次目录，避免主壳更新依赖本服务重启。
        assets = self.server.assets.resolve()
        asset = assets / path.lstrip('/')
        if not asset.resolve().is_relative_to(assets):
            raise PermissionError('资源路径无效')
        if not asset.is_file():
            asset = assets / 'index.html'
        return self.reply(asset.read_bytes(), content_type=mimetypes.guess_type(asset.name)[0] or 'application/octet-stream')

    def safe_dispatch(self):
        token = None
        try:
            # 本服务只监听回环；生产入口必须覆盖此头，后台任务复制请求上下文。
            values = self.headers.get_all('X-Real-IP', [])
            if len(values) > 1:
                raise ValueError('来源地址无效')
            token = CLIENT_IP.set(str(ip_address(values[0] if values else self.client_address[0])))
            self.dispatch()
        except PermissionError as error:
            self.reply({'error': str(error)},403)
        except PluginVersionChangedError as error:
            self.reply({'error': str(error)},409)
        except (ValueError, KeyError, TypeError, tarfile.TarError):
            self.reply({'error':'参数、插件包或版本无效'},400)
        except (OSError, TimeoutError, HTTPException):
            self.reply({'error':'插件服务暂不可用'},503)
        finally:
            if token is not None:
                CLIENT_IP.reset(token)

    do_GET = safe_dispatch
    do_POST = safe_dispatch
    def log_message(self, *_):
        pass


class BoundedServer(ThreadingHTTPServer):
    daemon_threads=True
    def __init__(self,*args,**kwargs):
        self.slots=threading.BoundedSemaphore(32)
        super().__init__(*args,**kwargs)
    def process_request(self,request,address):
        if not self.slots.acquire(blocking=False):
            request.close();return
        try:super().process_request(request,address)
        except Exception:self.slots.release();raise
    def process_request_thread(self,request,address):
        try:super().process_request_thread(request,address)
        finally:self.slots.release()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action',choices=['serve','install','status'])
    parser.add_argument('--root',type=Path,required=True)
    parser.add_argument('--gateway-port',type=int,default=18336)
    parser.add_argument('--port',type=int,default=18335)
    parser.add_argument('--origin',default='http://127.0.0.1:18335')
    parser.add_argument('--assets',type=Path,default=REPO/'frontend/dist')
    parser.add_argument('--local-access',type=Path)
    parser.add_argument('--plugin')
    parser.add_argument('--local-excel-wsl')
    parser.add_argument('--updater-socket',type=Path)
    args=parser.parse_args()
    if args.local_access and args.origin != 'http://127.0.0.1:18335':
        raise ValueError('便捷登录只允许本机测试')
    platform=Platform(args.root,args.gateway_port,args.local_excel_wsl,args.updater_socket)
    if args.action=='install':
        ids=[args.plugin] if args.plugin else list(platform.policies)
        for plugin_id in ids:
            platform.install(plugin_id)
        print(json.dumps({'installed':ids}))
    elif args.action=='status':
        print(json.dumps(platform.catalog('admin',True),ensure_ascii=False))
    else:
        server=BoundedServer(('127.0.0.1',args.port),Handler)
        server.platform,server.origin,server.assets,server.local_access=platform,args.origin,args.assets.absolute(),args.local_access
        print('Plugin platform listening at '+args.origin,flush=True)
        server.serve_forever()

if __name__=='__main__':
    main()

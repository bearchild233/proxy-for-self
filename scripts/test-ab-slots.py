"""Isolated WSL A/B gateway integration. Never accepts production addresses."""
import copy
import http.client
import json
import os
from pathlib import Path
import platform
import pwd
import secrets
import signal
import socket
import subprocess
import sys
import time
import threading
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(REPO/'services/updater'))
from slots import SlotController, SystemdCaddyDriver, atomic_json


def main():
    if 'microsoft' not in platform.release().lower() or os.geteuid()!=0:
        raise SystemExit('Run in local WSL as root')
    base=Path('/work/ab-stage');base.mkdir(exist_ok=True)
    builder=pwd.getpwnam('builder')
    os.chown(base,builder.pw_uid,builder.pw_gid);base.chmod(0o700)
    binary=Path('/work/proxy-for-self/backend/target/debug/codex-proxy-rs')
    caddy=REPO/'.build/platform-v2/caddy'
    if not binary.is_file() or not caddy.is_file():raise RuntimeError('Build gateway and supply local Caddy binary')
    for port in (18430,18431,18432,18433,18434,18480):
        with socket.socket() as probe:
            if probe.connect_ex(('127.0.0.1',port))==0:raise RuntimeError('A/B test port already occupied')
    env={k:v for k,v in os.environ.items() if not k.startswith(('CPR_','REDIS','PG'))}
    env.update(LANG='C.UTF-8', LC_ALL='C.UTF-8')
    credentials=json.loads(Path('/work/test-services/credentials.json').read_text())
    pg_env=dict(env,PGPASSWORD=credentials['postgres'])
    pg=['/usr/lib/postgresql/18/bin/psql','-h','127.0.0.1','-p','15432','-U','cprtest']
    def sql(query,database='postgres'):
        return subprocess.check_output([*pg,'-d',database,'-Atc',query],env=pg_env,text=True).strip()
    if sql("select 1 from pg_database where datname='cpr_ab_stage'")!='1':sql('create database cpr_ab_stage')
    def owned(path):os.chown(path,builder.pw_uid,builder.pw_gid)
    redis_secret=secrets.token_hex(24)
    vault=base/'vault.hex'
    if not vault.exists():vault.write_text(secrets.token_hex(32))
    owned(vault);vault.chmod(0o600)
    redis_config=base/'redis.conf'
    redis_config.write_text('\n'.join(['bind 127.0.0.1','port 18480','protected-mode yes','requirepass '+redis_secret,'save ""','appendonly no','maxmemory 32mb','maxmemory-policy noeviction','']))
    owned(redis_config);redis_config.chmod(0o600)
    processes=[]
    def read_exact(connection, size):
        data=b''
        while len(data)<size:
            part=connection.recv(size-len(data))
            if not part:raise RuntimeError('WebSocket closed during Caddy switch')
            data+=part
        return data
    class EchoWebSocket(BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*args):pass
        def do_GET(self):
            digest=hashlib.sha1((self.headers['Sec-WebSocket-Key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()
            self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade')
            self.send_header('Sec-WebSocket-Accept',base64.b64encode(digest).decode());self.end_headers()
            try:
                while True:
                    frame=read_exact(self.connection,2)
                    if frame[0]&15==8:break
                    length=frame[1]&127
                    if length>=126:raise RuntimeError('Unexpected fixture frame')
                    mask=read_exact(self.connection,4) if frame[1]&128 else b'\0'*4
                    payload=read_exact(self.connection,length)
                    payload=bytes(byte^mask[index%4] for index,byte in enumerate(payload))
                    self.connection.sendall(bytes([0x81,len(payload)])+payload)
            except (OSError,RuntimeError):pass
            self.close_connection=True
    echo=ThreadingHTTPServer(('127.0.0.1',18434),EchoWebSocket)
    echo_thread=threading.Thread(target=echo.serve_forever,daemon=True);echo_thread.start()
    def spawn(command,cwd,logname):
        with (base/logname).open('ab') as log:
            process=subprocess.Popen(command,cwd=cwd,env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=log,
                user=builder.pw_uid,group=builder.pw_gid,extra_groups=[],start_new_session=True)
        processes.append(process);return process
    def stop(process):
        if process.poll() is None:
            process.send_signal(signal.SIGTERM);process.wait(timeout=20)
    redis=spawn(['redis-server',str(redis_config)],base,'redis.log')
    config=copy.deepcopy(json.loads(Path('/work/plugin-stage/deploy/config.yaml').read_text()))
    config['store']['database']['url']='postgres://cprtest@127.0.0.1:15432/cpr_ab_stage'
    config['store']['database']['password']=credentials['postgres']
    config['store']['redis']={'url':'redis://127.0.0.1:18480/','password':redis_secret}
    config['store']['vault_key_file']=str(vault)
    access=base/'admin-secret'
    if not access.exists():access.write_text(secrets.token_hex(24));access.chmod(0o600)
    config['admin']['default_password']=access.read_text()
    config['api']['trusted_proxy_ips']=['127.0.0.1']
    config['openai']['pinned_cli_auto_update']=False
    for name,port in [('bootstrap',18432),('a',18430),('b',18431)]:
        directory=base/name
        for child in [directory,directory/'deploy',directory/'data',directory/'web',directory/'logs']:
            child.mkdir(exist_ok=True);owned(child)
        (directory/'web/index.html').write_text(name)
        cfg=copy.deepcopy(config)
        cfg['host']['listen']={'host':'127.0.0.1','port':port}
        cfg['host']['runtime_data_dir']=str(directory/'data')
        cfg['host']['logging']['file']['directory']=str(directory/'logs')
        cfg['api']['asset_directory']=str(directory/'web')
        if name!='bootstrap':cfg['store']['deployment_slot']=name
        path=directory/'deploy/config.yaml';path.write_text(json.dumps(cfg));owned(path);path.chmod(0o600)
    def request(port,path='/healthz',data=None,cookie=''):
        conn=http.client.HTTPConnection('127.0.0.1',port,timeout=3)
        try:
            conn.request('POST' if data is not None else 'GET',path,None if data is None else json.dumps(data),{'X-Real-IP':'127.0.0.1','Content-Type':'application/json','Origin':'http://127.0.0.1:18432','Cookie':cookie})
            response=conn.getresponse();return response.status,response.read(),response.getheader('Set-Cookie'),response.getheader('x-proxy-process')
        finally:conn.close()
    def ready(port):
        for _ in range(70):
            try:
                if request(port)[0]==204:return
            except OSError:pass
            time.sleep(.5)
        raise RuntimeError('Local slot never became ready')
    upstream=base/'gateway.caddy';upstream.write_text('reverse_proxy 127.0.0.1:18430 {\n stream_close_delay 15m\n}\n')
    caddyfile=base/'Caddyfile'
    caddyfile.write_text('''{
 admin 127.0.0.1:18433
 auto_https off
}
http://127.0.0.1:18432 {
 handle /probe {
  reverse_proxy 127.0.0.1:18434 {
   stream_close_delay 15m
  }
 }
 handle {
 route {
  import gateway.caddy
 }
 }
}
''')
    # JSON API pointer is fixed by this isolated fixture; production requires a dedicated @id.
    driver_config={'slots':{s:{'unit':'proxy-for-self-slot-'+s+'.service','port':p,'directory':str(base/s)} for s,p in [('a',18430),('b',18431)]},
        'caddy_admin_port':18433,'caddy_dial_path':'/config/apps/http/servers/srv0/routes/0/handle/0/routes/0/handle/0/upstreams/0/dial',
        'caddy_upstream_file':str(upstream),'verify_host':'127.0.0.1:18432','verify_port':18432,'ready_timeout':40}
    class LocalDriver(SystemdCaddyDriver):
        def __init__(self):super().__init__(driver_config);self.children={};self.broken=False
        def running(self,s):return s in self.children and self.children[s].poll() is None
        def prepare(self,s,r):pass
        def start(self,s):self.children[s]=spawn(['/bin/false'] if self.broken else [str(binary)],base/s,s+'.log')
        def stop(self,s):
            if s in self.children:stop(self.children[s])
        def verify(self,s,ingress=False):
            if self.broken:raise RuntimeError('intentional failed candidate')
            super().verify(s,ingress)
    driver=LocalDriver()
    report={}
    try:
        bootstrap=spawn([str(binary)],base/'bootstrap','bootstrap.log');ready(18432);stop(bootstrap)
        driver.start('a');ready(18430)
        proxy=spawn([str(caddy),'run','--config',str(caddyfile),'--adapter','caddyfile'],base,'caddy.log')
        ready(18432)
        ws=socket.create_connection(('127.0.0.1',18432),timeout=3)
        ws.sendall(b'GET /probe HTTP/1.1\r\nHost: 127.0.0.1:18432\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: YWJzbG90dGVzdGluZzEyMw==\r\n\r\n')
        headers=b''
        while not headers.endswith(b'\r\n\r\n'):headers+=read_exact(ws,1)
        assert b'101' in headers.split(b'\r\n')[0]
        def echo_check(message):
            payload=message.encode();mask=b'abcd'
            ws.sendall(bytes([0x81,0x80|len(payload)])+mask+bytes(byte^mask[index%4] for index,byte in enumerate(payload)))
            frame=read_exact(ws,2)
            assert frame[0]==0x81 and read_exact(ws,frame[1]&127)==payload
        echo_check('before')
        conn=http.client.HTTPConnection('127.0.0.1',18433,timeout=3)
        conn.request('GET','/config/');actual=json.loads(conn.getresponse().read());conn.close()
        def find_dial(value,path=''):
            if isinstance(value,dict):
                for key,child in value.items():
                    if key=='dial' and child=='127.0.0.1:18430':return '/config'+path+'/dial'
                    found=find_dial(child,path+'/'+key)
                    if found:return found
            if isinstance(value,list):
                for index,child in enumerate(value):
                    found=find_dial(child,path+'/'+str(index))
                    if found:return found
        driver.config['caddy_dial_path']=find_dial(actual)
        assert driver.config['caddy_dial_path']
        # 在切换期间循环访问稳定入口，必须全部成功。
        failures=[];done=threading.Event();count=[0]
        def poll():
            while not done.is_set():
                try:
                    if request(18432)[0]!=204:failures.append('status')
                except Exception:failures.append('connection')
                count[0]+=1;time.sleep(.08)
        monitor=threading.Thread(target=poll);monitor.start()
        state=base/'slots.json';atomic_json(state,{'active':'a','phase':'idle'})
        controller=SlotController(state,driver)
        original=driver.children['a'].pid
        driver.broken=True
        try:controller.deploy('bad')
        except RuntimeError:pass
        assert driver.children['a'].pid==original and driver.running('a') and driver.routed_slot()=='a'
        driver.broken=False
        controller.deploy('v2');assert driver.routed_slot()=='b' and not driver.running('a')
        echo_check('after-a-to-b')
        controller.deploy('v3');assert driver.routed_slot()=='a' and not driver.running('b')
        echo_check('after-b-to-a')
        ws.close()
        done.set();monitor.join();assert not failures,failures
        # 同一数据库中的登录资料跨轮换保留。
        status,body,cookie,_=request(18432,'/api/auth/login',{'mode':'admin','username':config['admin']['default_username'],'password':config['admin']['default_password']})
        assert status==200
        if cookie:request(18432,'/api/auth/logout',{},cookie.split(';')[0])
        report.update(realGatewaySlots=True,sharedDatabase='cpr_ab_stage',rotations=['a->b','b->a'],badCandidatePreservedActive=True,
            availabilityChecks=count[0],availabilityFailures=len(failures),inactiveProcessStopped=True,caddyReloadPreservedWebSocket=True,production=False)
    finally:
        if 'ws' in locals():ws.close()
        echo.shutdown();echo.server_close()
        if 'done' in locals():done.set()
        if 'monitor' in locals():monitor.join(timeout=5)
        for p in reversed(processes):
            if p.poll() is None:stop(p)
    output=REPO/'.build/platform-v2/ab-integration.json';output.write_text(json.dumps(report,indent=2));print(json.dumps(report))

if __name__=='__main__':main()

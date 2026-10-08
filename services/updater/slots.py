"""Fixed A/B slots: prepare inactive, verify, switch ingress, then drain previous.

Linux operator CLI; the JSON config is root-owned. Does not bootstrap legacy
single-instance installations or silently change database schema.
"""
import argparse
from contextlib import contextmanager
import fcntl
import http.client
import json
import os
from pathlib import Path
import subprocess
import time


def atomic_json(path, value):
    path = Path(path)
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as output:
        json.dump(value, output, indent=2)
        output.flush()
        os.fsync(output.fileno())
    temporary.chmod(0o600)
    temporary.replace(path)
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try: os.fsync(descriptor)
    finally: os.close(descriptor)


class SlotController:
    def __init__(self, state_file, driver):
        self.path = Path(state_file)
        self.driver = driver

    @contextmanager
    def locked(self):
        with self.path.with_suffix('.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            yield

    def save(self, state, phase):
        state['phase'] = phase
        atomic_json(self.path, state)

    def deploy(self, release):
        with self.locked():
            state = json.loads(self.path.read_text())
            if state['phase'] != 'idle':
                raise RuntimeError('Unfinished slot operation; inspect/resume before another deployment')
            old = state['active']
            new = 'b' if old == 'a' else 'a'
            if self.driver.routed_slot() != old or not self.driver.healthy(old):
                raise RuntimeError('Active slot and ingress do not match')
            if self.driver.running(new):
                raise RuntimeError('Inactive slot still running; do not replace it')
            state.update(previous=old, candidate=new, release=str(release))
            self.save(state, 'preparing')
            try:
                self.driver.prepare(new, release)
                self.driver.start(new)
                self.driver.verify(new, ingress=False)
                self.save(state, 'ready')
                self.driver.before_switch(old, new)
                # 写意图后才切入口，执行者崩溃后按真实路由恢复。
                self.save(state, 'switching')
                self.driver.switch(old, new)
                self.save(state, 'verifying')
                self.driver.verify(new, ingress=True)
            except Exception:
                self.recover(state)
                raise
            state['active'] = new
            self.save(state, 'draining')
            self.finish_drain(state)

    def finish_drain(self, state):
        new, old = state['active'], state['previous']
        if self.driver.routed_slot() != new or not self.driver.healthy(new):
            raise RuntimeError('Active slot unhealthy; keep previous process for diagnosis')
        self.driver.activate(new)
        # 此后失败只留下 draining 状态；不能回退到已经收到 SIGTERM 的旧进程。
        self.driver.stop(old)
        if self.driver.running(old):
            raise RuntimeError('Previous slot is still draining')
        self.save(state, 'idle')

    def boot(self):
        """开机只启动已确认槽；中断的切换先恢复最后确认的活动槽。"""
        with self.locked():
            state = json.loads(self.path.read_text())
            if state['phase'] not in {'idle','preparing','ready','switching','verifying','draining'}:
                raise RuntimeError('Slot state requires operator inspection')
            active = state['active']
            if not self.driver.running(active):self.driver.start(active)
            self.driver.verify(active, ingress=False)
            routed=self.driver.routed_slot()
            if routed != active:
                if routed not in {'a','b'}:raise RuntimeError('Unknown boot ingress')
                self.driver.switch(routed,active)
            self.driver.verify(active, ingress=True)
            self.driver.activate(active)
            other='b' if active=='a' else 'a'
            if self.driver.running(other):self.driver.stop(other)
            if self.driver.running(other):raise RuntimeError('Other slot has not drained')
            self.save(state,'idle')

    def recover(self, state):
        old, new = state['previous'], state['candidate']
        routed = self.driver.routed_slot()
        if routed == new:
            if not self.driver.healthy(old):
                self.save(state, 'attention')
                raise RuntimeError('Previous slot unavailable; preserving both slots')
            self.driver.switch(new, old)
            self.driver.verify(old, ingress=True)
        elif routed != old:
            self.save(state, 'attention')
            raise RuntimeError('Ingress changed externally; preserving both slots')
        if not self.driver.healthy(old):
            self.save(state, 'attention')
            raise RuntimeError('Original slot unhealthy; candidate retained for manual recovery')
        self.driver.stop(new)
        if self.driver.running(new):
            raise RuntimeError('Candidate still draining; recovery not complete')
        state['active'] = old
        self.save(state, 'idle')

    def resume(self):
        with self.locked():
            state = json.loads(self.path.read_text())
            if state['phase'] == 'draining': self.finish_drain(state)
            elif state['phase'] in {'preparing', 'ready', 'switching', 'verifying'}:
                self.recover(state)
            elif state['phase'] != 'idle':
                raise RuntimeError('Manual inspection required; no repeated restart')


class SystemdCaddyDriver:
    def __init__(self, config):
        self.config = config
        if set(config['slots']) != {'a','b'}: raise ValueError('Exactly slots a and b required')
        ports = [config['slots'][s]['port'] for s in ('a','b')]
        if any(not isinstance(p,int) or not 1024 <= p <= 65535 for p in ports) or len(set(ports)) != 2:
            raise ValueError('Slots require distinct unprivileged ports')
        for s, slot in config['slots'].items():
            if slot['unit'] != 'proxy-for-self-slot-'+s+'.service':raise ValueError('Unexpected slot unit')

    def command(self, args, timeout=30):
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        if result.returncode: raise RuntimeError('Slot service command failed; inspect journal')
        return result.stdout.strip()

    def running(self, slot):
        return self.command(['systemctl','show',self.config['slots'][slot]['unit'],'-p','ActiveState','--value']) not in {'inactive','failed'}

    def prepare(self, slot, release):
        root = Path(self.config['release_root']).resolve(strict=True)
        release = Path(release).resolve(strict=True)
        if release.parent != root or not (release/'bin/api-hub').is_file():
            raise ValueError('Release must be an existing verified directory inside release_root')
        # 由打包阶段产出的元数据必须声明 A/B 合同；旧单实例二进制不得进入槽位。
        metadata = json.loads((release/'slot-manifest.json').read_text())
        import hashlib
        if metadata.get('protocol') != 1 or metadata.get('binarySha256') != hashlib.sha256((release/'bin/api-hub').read_bytes()).hexdigest():
            raise ValueError('Slot binary manifest mismatch')
        directory = Path(self.config['slots'][slot]['directory'])
        configuration = json.loads((directory/'deploy/config.yaml').read_text())
        if configuration['store'].get('deployment_slot') != slot or configuration['host']['listen'] != {'host':'127.0.0.1','port':self.config['slots'][slot]['port']}:
            raise ValueError('Slot identity/listen config mismatch')
        temporary = directory/'.current-next'
        temporary.unlink(missing_ok=True)
        temporary.symlink_to(release)
        temporary.replace(directory/'current')

    def start(self, slot):
        self.command(['systemctl','start',self.config['slots'][slot]['unit']])

    def before_switch(self, old, new):
        """有界重叠前检查余量，繁忙时保留当前入口，不降低用户运行配置。"""
        minimum=self.config.get('minimum_available_mib',0)
        maximum=self.config.get('maximum_draining_mib',0)
        if minimum:
            fields=dict(line.split(':',1) for line in Path('/proc/meminfo').read_text().splitlines())
            available=int(fields['MemAvailable'].split()[0])*1024
            if available < minimum*1024*1024:
                raise RuntimeError('Insufficient free memory for slot overlap; active slot preserved')
        if maximum:
            current=int(self.command(['systemctl','show',self.config['slots'][old]['unit'],'-p','MemoryCurrent','--value']))
            if current > maximum*1024*1024:
                raise RuntimeError('Previous slot is too large to overlap safely; retry when quieter')

    def activate(self, slot):
        if not self.config.get('current_link'):return
        target=(Path(self.config['slots'][slot]['directory'])/'current').resolve(strict=True)
        if target.parent != Path(self.config['release_root']).resolve(strict=True):
            raise RuntimeError('Active release is outside release root')
        link=Path(self.config['current_link']);temporary=link.with_name('.current-slot-next')
        temporary.unlink(missing_ok=True);temporary.symlink_to(target);temporary.replace(link)

    def stop(self, slot):
        self.command(['systemctl','stop',self.config['slots'][slot]['unit']], timeout=self.config.get('drain_timeout',660))

    def healthy(self, slot):
        connection = http.client.HTTPConnection('127.0.0.1',self.config['slots'][slot]['port'],timeout=2)
        try:
            connection.request('GET','/healthz')
            response=connection.getresponse();response.read()
            return response.status==204
        except (OSError,http.client.HTTPException):return False
        finally:connection.close()

    def verify(self, slot, ingress=False):
        deadline=time.monotonic()+self.config.get('ready_timeout',60)
        consecutive=0
        while time.monotonic()<deadline:
            okay=self.healthy(slot) and (not ingress or (self.routed_slot()==slot and self.ingress_matches(slot)))
            consecutive=consecutive+1 if okay else 0
            if consecutive>=3:return
            time.sleep(1)
        raise RuntimeError('Candidate did not pass readiness checks')

    def ingress_matches(self, slot):
        def identity(port):
            connection=http.client.HTTPConnection('127.0.0.1',port,timeout=3)
            try:
                connection.request('GET','/healthz',headers={'Host':self.config['verify_host']})
                response=connection.getresponse();response.read()
                return response.getheader('x-proxy-process') if response.status==204 else None
            except (OSError,http.client.HTTPException):return None
            finally:connection.close()
        candidate=identity(self.config['slots'][slot]['port'])
        return candidate is not None and identity(self.config['verify_port'])==candidate

    def caddy(self, method='GET', value=None, etag=None, path=None):
        conn=http.client.HTTPConnection('127.0.0.1',self.config['caddy_admin_port'],timeout=10)
        try:
            headers={'Content-Type':'application/json'}
            if etag:headers['If-Match']=etag
            conn.request(method,path or self.config['caddy_dial_path'],None if value is None else json.dumps(value),headers)
            response=conn.getresponse();body=response.read();tag=response.getheader('Etag')
            if response.status!=200:raise RuntimeError('Caddy rejected route change; no service stop')
            return json.loads(body) if body else None,tag
        finally:conn.close()

    def dial(self, slot):return '127.0.0.1:'+str(self.config['slots'][slot]['port'])

    def routed_slot(self):
        value,_=self.caddy()
        return next((s for s in ('a','b') if value==self.dial(s)),None)

    def switch(self, old, new):
        # PATCH 也会卸载旧 Caddy 配置；缺少延迟会直接关闭已有 WebSocket。
        parent = self.config['caddy_dial_path'].removesuffix('/upstreams/0/dial')
        if parent == self.config['caddy_dial_path']:
            raise RuntimeError('Expected a dedicated reverse_proxy upstream path')
        proxy, _ = self.caddy(path=parent)
        if proxy.get('stream_close_delay', 0) < self.config.get('drain_timeout',660) * 1_000_000_000:
            raise RuntimeError('Caddy stream_close_delay must cover the entire drain window')
        value,etag=self.caddy()
        if value!=self.dial(old) or not etag:raise RuntimeError('Ingress changed concurrently')
        # 独立导入片段是磁盘入口事实；仅替换唯一上游，其他站点不修改。
        path=Path(self.config['caddy_upstream_file'])
        before=path.read_text()
        if before.count(self.dial(old))==1:
            after=before.replace(self.dial(old),self.dial(new))
        elif before.count(self.dial(new))==1:
            after=before  # 执行者在 API 切换与磁盘替换之间退出后的恢复。
        else:raise RuntimeError('Caddy disk configuration mismatch')
        temporary=path.with_suffix('.next')
        with temporary.open('w') as output:
            output.write(after);output.flush();os.fsync(output.fileno())
        temporary.chmod(path.stat().st_mode & 0o777)
        # Caddy API 单请求原子切换且用 ETag 防止覆盖其他管理者的修改。
        self.caddy('PATCH',self.dial(new),etag)
        try:temporary.replace(path)
        except OSError:
            current,tag=self.caddy()
            if current==self.dial(new):self.caddy('PATCH',self.dial(old),tag)
            raise


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config',type=Path,required=True)
    parser.add_argument('action',choices=['deploy','resume','boot','status'])
    parser.add_argument('--release',type=Path)
    args=parser.parse_args();config=json.loads(args.config.read_text())
    controller=SlotController(config['state_file'],SystemdCaddyDriver(config))
    if args.action=='deploy':
        if not args.release:parser.error('--release required')
        controller.deploy(args.release)
    elif args.action=='resume':controller.resume()
    elif args.action=='boot':controller.boot()
    print(Path(config['state_file']).read_text())

if __name__=='__main__':main()

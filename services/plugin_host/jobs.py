"""有界后台任务；每项任务锁定包版本，后台逻辑随插件包独立更新。"""
import hashlib
from contextvars import copy_context
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
import uuid


class Jobs:
    def __init__(self, platform):
        self.platform = platform
        self.lock = threading.RLock()
        self.jobs = {}
        self.slots = threading.BoundedSemaphore(4)

    def start(self, plugin_id, version, data, cookie, role, origin):
        if role != 'admin' or not self.platform.store.state(plugin_id)['enabled']:
            raise PermissionError('插件任务不可用')
        manifest = self.platform.store.manifest(plugin_id, version)
        if not manifest.get('worker') or not self.slots.acquire(blocking=False):
            raise ValueError('后台任务不可用或繁忙')
        try:
            lease = self.platform.store.lease(plugin_id, version)
        except Exception:
            self.slots.release()
            raise
        task_id = uuid.uuid4().hex
        record = {'id':task_id,'pluginId':plugin_id,'version':version,'status':'running','createdAt':time.time(), 'accountId':data.get('accountId')}
        with self.lock:
            self.jobs = {key:value for key,value in self.jobs.items() if value['record']['status']=='running' or value['record']['createdAt'] > time.time()-3600}
            if len(self.jobs) >= 128:
                finished = [key for key,value in self.jobs.items() if value['record']['status'] != 'running']
                if finished:
                    del self.jobs[finished[0]]
            self.jobs[task_id] = {'owner':hashlib.sha256(cookie.encode()).hexdigest(),'record':record,'cancel':threading.Event()}
        threading.Thread(target=copy_context().run,args=(self.run,task_id,manifest,lease,data,cookie,role,origin),daemon=True).start()
        return dict(record)

    def get(self, task_id, cookie, cancel=False):
        with self.lock:
            job = self.jobs.get(task_id)
            if not job or job['owner'] != hashlib.sha256(cookie.encode()).hexdigest():
                raise PermissionError('任务不存在或无权访问')
            if cancel:
                job['cancel'].set()
            return dict(job['record'])

    def run(self, task_id, manifest, lease, data, cookie, role, origin):
        job = self.jobs[task_id]
        record = job['record']
        process = None
        try:
            worker = self.platform.store.directory(record['pluginId'])/'releases'/record['version']/manifest['worker']
            # 不继承凭据环境。插件属于管理员安装的受信任代码；生产服务另由 systemd 隔离。
            env = {key:os.environ[key] for key in ('SYSTEMROOT','WINDIR','LANG') if key in os.environ}
            process = subprocess.Popen([sys.executable,'-I','-u',str(worker)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,cwd=worker.parent,env=env,text=True,encoding='utf-8')
            lines = queue.Queue(maxsize=16)
            def read():
                try:
                    while True:
                        line = process.stdout.readline(262145)
                        if not line:
                            break
                        if len(line)>262144:
                            break
                        lines.put(line,timeout=1)
                except (OSError,queue.Full):
                    pass
                finally:
                    try: lines.put(None,timeout=1)
                    except queue.Full: pass
            threading.Thread(target=read,daemon=True).start()
            process.stdin.write(json.dumps(data,ensure_ascii=False)+'\n')
            process.stdin.flush()
            deadline = time.monotonic()+90
            calls = 0
            while time.monotonic()<deadline and not job['cancel'].is_set():
                try: line = lines.get(timeout=.2)
                except queue.Empty: continue
                if line is None:
                    raise ValueError('插件任务提前结束')
                message = json.loads(line)
                if 'result' in message:
                    if not isinstance(message['result'],dict):raise ValueError('任务结果必须为对象')
                    record.update(status='succeeded',result=message['result'])
                    break
                if 'call' not in message or calls >= 3:
                    raise ValueError('插件调用超过限制')
                if record['pluginId']=='account-diagnostics' and message.get('input',{}).get('accountId') != data.get('accountId'):
                    raise ValueError('账号范围无效')
                calls += 1
                # 运行中的任务可排空；仅授权任务启动时已经校验的版本。
                status,body,_ = self.platform.call(record['pluginId'],record['version'],message['call'],message.get('input',{}),cookie,role,origin,draining=True)
                result = json.loads(body)
                process.stdin.write(json.dumps({'ok':status==200,'value':result.get('data',result) if status==200 else None})+'\n')
                process.stdin.flush()
            if record['status']=='running':
                record.update(status='cancelled' if job['cancel'].is_set() else 'failed',error='任务已取消' if job['cancel'].is_set() else '任务超时')
        except Exception:
            record.update(status='failed',error='诊断失败，请确认账号状态或稍后重试')
        finally:
            if process:
                if process.poll() is None: process.kill()
                process.wait(timeout=5)
                process.stdin.close()
                process.stdout.close()
            record['finishedAt']=time.time()
            try:
                if record['pluginId']=='account-diagnostics':
                    result=record.get('result',{})
                    self.platform.history(append={key:value for key,value in {**record,'result':{key:value for key,value in result.items() if key in {'phase','status','stateReturned','durationMs','at','classification','bootstrapPerformed','failed','reason'}}}.items() if key!='version'})
            finally:
                try:self.platform.store.lease(record['pluginId'],record['version'],lease,release=True)
                finally:self.slots.release()

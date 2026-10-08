"""将可选 Worker 与 UI 准入作为同一管理操作，后台排空不阻塞面板。"""
import json
from contextvars import copy_context
import socket
from http.client import HTTPConnection
import os
import subprocess
import threading
import time
from pathlib import Path, PureWindowsPath


class Lifecycle:
    def __init__(self, platform, local_excel=None, updater_socket=None):
        self.platform=platform
        self.local_excel=local_excel
        self.updater_socket=updater_socket
        self.lock=threading.RLock()
        self.journal=platform.root/'lifecycle.json'
        self.operations=json.loads(self.journal.read_text(encoding='utf-8')) if self.journal.exists() else {}
        pending=[plugin_id for plugin_id,operation in self.operations.items() if operation.get('status')=='running']
        for plugin_id in pending:
            threading.Thread(target=self.recover,args=(plugin_id,),daemon=True).start()

    def recover(self, plugin_id):
        try:
            deadline=time.monotonic()+650
            while True:
                result=self.excel('status')
                if result.get('operation',{}).get('status')!='running':break
                if time.monotonic()>deadline:raise ValueError('Worker recovery timeout')
                time.sleep(1)
            operation=self.operations[plugin_id]
            if not result.get('installed'):
                self.platform.store.action(plugin_id,'uninstall')
            else:
                if not self.platform.store.state(plugin_id)['current']:
                    self.platform.install(plugin_id)
                self.platform.store.action(plugin_id,'enable' if result.get('enabled') else 'disable')
            operation.update(status='succeeded',message='已与独立安装器恢复状态同步')
        except Exception:
            # 无法确认 Worker 状态时不开放入口；重试启用可恢复，不能假装已经完成。
            if self.platform.store.state(plugin_id)['enabled']:
                self.platform.store.action(plugin_id,'disable')
            self.operations[plugin_id].update(status='failed',message='恢复时未能确认 Worker 状态，请重试启用或检查安装器')
        finally:self.save()

    def updater(self,method,path,value=None):
        connection=HTTPConnection('localhost',timeout=15)
        try:
            connection.sock=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
            connection.sock.settimeout(15)
            connection.sock.connect(str(self.updater_socket))
            connection.request(method,path,None if value is None else json.dumps(value),{'Content-Type':'application/json'})
            response=connection.getresponse();data=json.loads(response.read(1048576))
            if response.status not in {200,202}:raise ValueError('安装器拒绝操作')
            return data
        finally:connection.close()

    def save(self):
        temporary=self.journal.with_suffix('.tmp')
        with self.lock:
            temporary.write_text(json.dumps(self.operations,ensure_ascii=False),encoding='utf-8')
            temporary.replace(self.journal)

    def excel(self, action, cookie='', origin=None):
        if self.local_excel:
            script=PureWindowsPath(Path(__file__).resolve().parents[2]/'scripts/plugin-stage-excel.py')
            linux_script='/mnt/'+script.drive[0].lower()+'/'+('/'.join(script.parts[1:]))
            command=['wsl.exe','-d',self.local_excel,'-u','root','--','python3',linux_script,action]
            result=subprocess.run(command,capture_output=True,text=True,encoding='utf-8',timeout=650,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
            if result.returncode:raise ValueError('本机 Excel Worker 操作失败，请检查测试日志')
            return json.loads(result.stdout)
        if action!='status':
            if self.updater_socket:self.updater('POST','/plugins/action',{'id':'excel-bridge','action':action})
            else:
                status,body,_=self.platform.forward('POST','/api/admin/system/plugins/action',cookie,{'id':'excel-bridge','action':action},origin)
                if status!=200:raise ValueError('Excel Worker 操作未被接受')
        deadline=time.monotonic()+650
        while True:
            if self.updater_socket:items=self.updater('GET','/plugins')
            else:
                status,body,_=self.platform.forward('GET','/api/admin/system/plugins',cookie)
                if status!=200:raise ValueError('Excel Worker 状态不可用')
                items=json.loads(body).get('data',[])
            item=next((p for p in items if p['id']=='excel-bridge'),None)
            if not item:raise ValueError('Excel Worker 尚未注册')
            if action=='status' or item.get('operation',{}).get('status')!='running':return item
            if time.monotonic()>deadline:raise ValueError('Excel Worker 操作仍在执行')
            time.sleep(1)

    def begin(self, plugin_id, action, cookie, origin, expected_current=None, expected_previous=None):
        self.platform.store.directory(plugin_id)
        if plugin_id!='excel-bridge':
            return self.platform.install(plugin_id) if action in {'install','update'} else self.platform.store.action(plugin_id,action,expected_current,expected_previous)
        if action not in {'install','update','enable','disable','uninstall'}:
            raise ValueError('Excel Worker 仅支持安装、更新、启用、停用与卸载；更新失败自动恢复')
        with self.lock:
            if self.operations.get(plugin_id,{}).get('status')=='running':raise ValueError('插件操作正在执行')
            before=self.platform.store.state(plugin_id)
            self.operations[plugin_id]={'status':'running','message':'等待 Worker 排空并验证','before':before,'action':action}
            self.save()
            if before['enabled']:self.platform.store.action(plugin_id,'disable')
            threading.Thread(target=copy_context().run,args=(self.execute,plugin_id,action,before,cookie,origin),daemon=True).start()
            return self.operations[plugin_id]

    def execute(self, plugin_id, action, before, cookie, origin):
        try:
            result=self.excel(action,cookie,origin)
            if result.get('operation',{}).get('status')=='failed':raise ValueError('Worker 操作失败，已尝试恢复原状态')
            if action in {'install','update'}:
                if not before['current']:self.platform.install(plugin_id)
                if result.get('enabled'):self.platform.store.action(plugin_id,'enable')
                else:self.platform.store.action(plugin_id,'disable')
            else:self.platform.store.action(plugin_id,action)
            self.operations[plugin_id]={'status':'succeeded','message':'插件操作完成'}
        except Exception:
            # Worker 的事务安装器负责二进制恢复，UI 恢复同一启用状态。
            if before['current']:
                with self.platform.store.mutation():self.platform.store.save(plugin_id,before)
            self.operations[plugin_id]={'status':'failed','message':'插件操作失败，请检查 Worker 状态；原界面版本已保留'}

        finally:
            self.save()

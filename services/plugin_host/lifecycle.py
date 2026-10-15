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

REPO = Path(__file__).resolve().parents[2]
# 服务插件操作等待 updater 完成的时限：240 秒（006 推断，007 经设计负责人接受）。
SERVICE_TIMEOUT_SECONDS = 240
# 轮询 updater 操作状态的间隔。TODO(design): 任务未给出，需要设计负责人确认。
SERVICE_POLL_SECONDS = 1


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
            if self.operations[plugin_id].get('kind')=='service':
                # 控制面中断时无法确认 updater 的进度；不自动重放，状态以安装器与存储为准，请人工核对。
                self.operations[plugin_id].update(status='failed',message='控制面重启时服务操作中断，请核对服务状态后重试')
                continue
            threading.Thread(target=self.recover,args=(plugin_id,),daemon=True).start()
        if any(self.operations[plugin_id].get('kind')=='service' for plugin_id in pending):
            self.save()

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
        if plugin_id!='excel-bridge' and self.service_plugin(plugin_id, action):
            return self.begin_service(plugin_id, action, expected_current, expected_previous)
        if plugin_id!='excel-bridge':
            result = self.platform.install(plugin_id) if action in {'install','update'} else self.platform.store.action(plugin_id,action,expected_current,expected_previous)
            return result
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

    # ----- 服务插件（带 service 段）：存储切换 → updater 切换 → 失败恢复存储 -----

    def has_service(self, plugin_id, version):
        return self.platform.store.manifest(plugin_id, version).get('service') is not None

    def service_plugin(self, plugin_id, action):
        """安装与更新看目标包或当前版本是否带服务段；其余操作看当前与上一版本。"""
        if action in {'install','update'}:
            package=REPO/'.build/plugins'/plugin_id/'package/plugin.json'
            if package.is_file() and 'service' in json.loads(package.read_text(encoding='utf-8')):
                return True
        state=self.platform.store.state(plugin_id)
        return any(self.has_service(plugin_id,version) for version in (state['current'],state['previous']) if version)

    def require_service_ready(self, plugin_id):
        if not self.updater_socket:
            raise ValueError('服务插件需要独立安装器，当前未配置')
        if self.operations.get(plugin_id,{}).get('status')=='running':
            raise ValueError('插件操作正在执行')

    def begin_service(self, plugin_id, action, expected_current=None, expected_previous=None):
        store=self.platform.store
        with self.lock:
            self.require_service_ready(plugin_id)
            before=store.state(plugin_id)
            # 先完成存储侧的包或状态切换（与非服务插件相同），再交给 updater 执行服务切换。
            if action in {'install','update'}:
                self.platform.install(plugin_id)
            elif action in {'enable','disable'}:
                store.action(plugin_id,action)
            elif action=='uninstall':
                # 先关闭准入；卸载成功后才删除存储中的版本。
                store.action(plugin_id,'disable')
            elif action=='rollback':
                store.action(plugin_id,'rollback',expected_current,expected_previous)
            else:
                raise ValueError('服务插件不支持此操作')
            return self.launch_service(plugin_id,action,before)

    def launch_service(self, plugin_id, action, before):
        """记录运行中的服务操作并在后台等待 updater；启动失败时恢复 before 的存储状态。"""
        with self.lock:
            try:
                self.require_service_ready(plugin_id)
                self.operations[plugin_id]={'status':'running','kind':'service','action':action,'message':'等待安装器切换服务','before':before}
                self.save()
            except Exception:
                self.restore_service(plugin_id,before)
                raise
        threading.Thread(target=self.execute_service,args=(plugin_id,action,before),daemon=True).start()
        return self.operations[plugin_id]

    def execute_service(self, plugin_id, action, before):
        try:
            operation_id=self.service_request(plugin_id,action,before)
            if operation_id:
                self.wait_service(operation_id)
            if action=='uninstall':
                self.platform.store.action(plugin_id,'uninstall')
            self.operations[plugin_id]={'status':'succeeded','kind':'service','action':action,'message':'插件操作完成'}
        except Exception as error:
            message='插件操作失败：'+str(error) if isinstance(error,(ValueError,RuntimeError)) else '插件操作失败，已恢复原界面版本与状态'
            try:
                self.restore_service(plugin_id,before)
            except Exception:
                # 恢复本身失败时仍必须结束操作记录，避免永久显示运行中。
                message+='；恢复存储状态失败，请人工核对'
            self.operations[plugin_id]={'status':'failed','kind':'service','action':action,'message':message}
        finally:
            self.save()

    def service_request(self, plugin_id, action, before):
        """按存储中的最新状态决定发给 updater 的操作；无需服务切换时返回 None。"""
        store=self.platform.store
        after=store.state(plugin_id)
        if action in {'install','update'}:
            if after['current'] and self.has_service(plugin_id,after['current']):
                # 存储未启用时新版本只落盘，不启动服务；下次启用时由 updater 按该版本启动。
                return self.post_service(plugin_id,'activate',after['current']) if after['enabled'] else None
            if before['current'] and self.has_service(plugin_id,before['current']):
                # 新版本不再带服务段：停止旧服务实例，界面版本照常。
                return self.post_service(plugin_id,'disable')
            return None
        if action=='enable':
            return self.post_service(plugin_id,'enable',after['current'])
        if action=='disable':
            return self.post_service(plugin_id,'disable')
        if action=='rollback':
            return self.post_service(plugin_id,'rollback')
        if action=='uninstall':
            return self.post_service(plugin_id,'uninstall')
        raise ValueError('服务插件不支持此操作')

    def post_service(self, plugin_id, action, version=None):
        body={'id':plugin_id,'action':action}
        if version:
            body['version']=version
        return self.updater('POST','/services/action',body)['operation_id']

    def wait_service(self, operation_id):
        deadline=time.monotonic()+SERVICE_TIMEOUT_SECONDS
        while True:
            operation=self.updater('GET','/services/status').get('operation',{})
            if operation.get('id')==operation_id and operation.get('status')!='running':
                if operation.get('status')=='succeeded':
                    return
                raise ValueError(operation.get('message') or '安装器操作失败')
            if time.monotonic()>=deadline:
                raise ValueError('安装器操作仍在执行')
            time.sleep(SERVICE_POLL_SECONDS)

    def restore_service(self, plugin_id, before):
        """把存储状态恢复到操作前；上一版本目录若已被清理则不再引用。"""
        store=self.platform.store
        state=dict(before)
        if state.get('previous') and not (store.directory(plugin_id)/'releases'/state['previous']).is_dir():
            state['previous']=None
        with store.mutation():
            store.save(plugin_id,state)
            store.cleanup(plugin_id,state)

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

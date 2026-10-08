"""Local-only role, optional lifecycle and configuration checks; no upstream account calls."""
import http.cookiejar
import json
import subprocess
import time
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import build_opener, HTTPCookieProcessor, Request

BASE='http://127.0.0.1:18335'
ROOT=Path(__file__).resolve().parents[1]


def client():return build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))


def call(opener,path,data=None,expected=200):
    req=Request(BASE+path,data=None if data is None else json.dumps(data).encode(),headers={'Origin':BASE,'Content-Type':'application/json'})
    try:response=opener.open(req,timeout=20)
    except HTTPError as error:response=error
    with response:
        value=json.loads(response.read())
        assert (200<=response.status<300 if expected==200 else response.status==expected),(path,response.status)
        return value.get('data',value) if isinstance(value,dict) else value


def wait_excel(admin):
    for _ in range(120):
        item=next(p for p in call(admin,'/api/plugin-platform/manage') if p['id']=='excel-bridge')
        if item.get('operation',{}).get('status')!='running':
            assert item['operation']['status']=='succeeded',item['operation']
            return item
        time.sleep(.5)
    raise AssertionError('local Excel operation timeout')


def main():
    admin=client();call(admin,'/api/plugin-platform/local-login',{})
    catalog={p['id']:p for p in call(admin,'/api/plugin-platform/manage')}
    def prefix(id):return '/api/plugin-platform/call/'+id+'/'+catalog[id]['contentVersion']+'/'
    diagnostic=prefix('account-diagnostics')
    templates=call(admin,diagnostic+'templates.read',{})
    try:
        edited=[dict(row) for row in templates];edited[0]['prompt']='local persistence check'
        call(admin,diagnostic+'templates.write',{'items':edited})
        assert call(admin,diagnostic+'templates.read',{})==edited
    finally:call(admin,diagnostic+'templates.write',{'items':templates})
    call(admin,'/api/plugin-platform/action',{'id':'account-diagnostics','action':'disable'})
    try:
        assert 'account-diagnostics' not in {p['id'] for p in call(admin,'/api/plugin-platform/catalog')}
        call(admin,diagnostic+'templates.read',{},403)
    finally:call(admin,'/api/plugin-platform/action',{'id':'account-diagnostics','action':'enable'})
    for leftover in call(admin,prefix('keys')+'api-keys.get.admin.client-keys',{'limit':100})['items']:
        if leftover['name']=='临时权限验证':call(admin,prefix('keys')+'api-keys.post.admin.client-keys.delete',{'id':leftover['id']})
    key=call(admin,prefix('keys')+'api-keys.post.admin.client-keys.create',{
        'name':'临时权限验证','label':None,'groupIds':[],'maxConcurrency':1,'requestsPerMinute':5,
        'dailyLimitUsd':'0','weeklyLimitUsd':'0','openaiClientProfileOverride':None,'xaiClientProfileOverride':None})
    try:
        user=client();call(user,'/api/auth/login',{'mode':'key','apiKey':key['plaintextKey']})
        available=call(user,'/api/plugin-platform/catalog')
        assert 'client-access' in {p['id'] for p in available}
        call(user,'/api/plugin-platform/manage',expected=403)
        call(user,prefix('keys')+'api-keys.get.admin.client-keys',{'limit':20},403)
        client_plugin=next(p for p in available if p['id']=='client-access')
        call(user,'/api/plugin-platform/page/client-access/'+client_plugin['contentVersion']+'/main')
    finally:call(admin,prefix('keys')+'api-keys.post.admin.client-keys.delete',{'id':key['id']})
    call(admin,'/api/plugin-platform/action',{'id':'excel-bridge','action':'disable'})
    try:
        assert not wait_excel(admin)['enabled']
        assert 'excel-bridge' not in {p['id'] for p in call(admin,'/api/plugin-platform/catalog')}
    finally:
        call(admin,'/api/plugin-platform/action',{'id':'excel-bridge','action':'enable'})
        assert wait_excel(admin)['enabled']
    code="from pathlib import Path;import json;p=int(Path('/work/plugin-stage/gateway.pid').read_text());print(json.dumps([p,Path(f'/proc/{p}/stat').read_text().split()[21]]))"
    identity=lambda:subprocess.check_output(['wsl.exe','-d','ApiHubBuild-20260928','--','python3','-c',code],text=True).strip()
    before=identity()
    call(admin,'/api/plugin-platform/action',{'id':'excel-bridge','action':'update'})
    assert wait_excel(admin)['enabled']
    assert before==identity()
    status=call(admin,prefix('excel-bridge')+'excel.status',{})
    assert status['enabled'] and status['installed']
    print(json.dumps({'keyRoleGuard':True,'keyImportAvailable':True,'templatePersistence':True,
                      'disabledRpcDenied':True,'excelDisableEnable':True,'excelWorkerUpdate':True,'gatewayUnchanged':True,'production':False}))


if __name__=='__main__':main()

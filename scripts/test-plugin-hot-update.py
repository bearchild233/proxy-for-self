"""Verify independent UI update/rollback against the local platform and unchanged gateway."""
import hashlib
import http.cookiejar
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from urllib.error import HTTPError
from urllib.request import build_opener,HTTPCookieProcessor,Request,urlopen

REPO=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(REPO/'services/plugin_host'))
from server import package_directory
BASE='http://127.0.0.1:18335'
client=build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))
def call(path,data=None,expected=200):
    req=Request(BASE+path,data=None if data is None else json.dumps(data).encode(),headers={'Content-Type':'application/json','Origin':BASE})
    try:response=client.open(req,timeout=15)
    except HTTPError as error:response=error
    with response:
        body=json.loads(response.read());assert response.status==expected,(response.status,body)
        return body.get('data',body) if isinstance(body,dict) else body

def identity():
    code="from pathlib import Path;import json;p=int(Path('/work/plugin-stage/gateway.pid').read_text());print(json.dumps({'pid':p,'start':Path(f'/proc/{p}/stat').read_text().split()[21]}))"
    return json.loads(subprocess.check_output(['wsl.exe','-d','ApiHubBuild-20260928','--','python3','-c',code],text=True))

call('/api/plugin-platform/local-login',{})
before=identity();health=[];stop=threading.Event()
def monitor():
    while not stop.is_set():
        try:
            with urlopen('http://127.0.0.1:18336/healthz',timeout=2) as response:health.append(response.status in (200,204))
        except OSError:health.append(False)
        stop.wait(.05)
thread=threading.Thread(target=monitor);thread.start()
try:
    group=next(item for item in call('/api/plugin-platform/catalog') if item['id']=='groups')
    old=group['contentVersion']
    page=call('/api/plugin-platform/page/groups/'+old+'/main')
    with tempfile.TemporaryDirectory() as temp:
        directory=Path(temp)/'package';(directory/'ui').mkdir(parents=True)
        manifest=json.loads((REPO/'.build/plugins/groups/package/plugin.json').read_text(encoding='utf-8'))
        entry=manifest['pages'][0]['entry']
        html=page['html']+('\n// independent-update-verification\n' if manifest.get('runtime')=='vue-component' else '<!-- independent-update-verification -->')
        (directory/entry).write_bytes(html.encode())
        (directory/'LICENSE').write_bytes((REPO/'LICENSE').read_bytes())
        manifest['resources']={name:hashlib.sha256((directory/name).read_bytes()).hexdigest() for name in [entry,'LICENSE']}
        (directory/'plugin.json').write_text(json.dumps(manifest))
        archive=Path(temp)/'groups.tar.gz';package_directory(directory,archive)
        raw=archive.read_bytes()
        import base64
        payload={'package':base64.b64encode(raw).decode(),'sha256':hashlib.sha256(raw).hexdigest()}
        result=call('/api/plugin-platform/upload',payload)
        assert result['current']!=old
        assert call('/api/plugin-platform/page/groups/'+old+'/main')['html']==page['html']
        call('/api/plugin-platform/upload',{**payload,'sha256':'0'*64},400)
        call('/api/plugin-platform/action',{'id':'groups','action':'disable'},400)
        restored=call('/api/plugin-platform/action',{'id':'groups','action':'rollback'})
        assert restored['current']==old
        call('/api/plugin-platform/lease',{'id':'groups','version':old,'lease':page['lease'],'release':True})
    after=identity();assert before==after
finally:
    stop.set();thread.join()
assert health and all(health),health
report={'gateway':before,'gatewayUnchanged':True,'healthChecks':len(health),'badPackageRejected':True,'requiredProtection':True,'oldPagePreserved':True,'rollback':True,'production':False}
(REPO/'.build/plugin-stage/verification.json').write_text(json.dumps(report,indent=2))
print(json.dumps(report))

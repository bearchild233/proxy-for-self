"""Exercise one due pricing sync in isolated localhost staging, restoring overrides."""
import http.cookiejar
import json
from pathlib import Path
import time
from urllib.request import Request, build_opener, HTTPCookieProcessor

ROOT = Path(__file__).resolve().parents[1]
BASE = 'http://127.0.0.1:18335'
client = build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))

def request(path, data=None):
    with client.open(Request(BASE+path, data=None if data is None else json.dumps(data).encode(),
                            headers={'Origin':BASE,'Content-Type':'application/json'}), timeout=45) as response:
        result=json.loads(response.read())
        return result.get('data',result) if isinstance(result,dict) else result

def main():
    request('/api/plugin-platform/local-login',{})
    plugin=next(x for x in request('/api/plugin-platform/catalog') if x['id']=='pricing')
    prefix='/api/plugin-platform/call/pricing/'+plugin['contentVersion']+'/'
    def call(operation,data=None):return request(prefix+operation,data or {})
    original=call('pricing.get.admin.settings.pricing')['overrides'].get('openai',{}).get('gpt-5')
    try:
        call('pricing.post.admin.settings.pricing.update',{'provider':'openai','models':['gpt-5'],'change':{'action':'multiplier','multiplierBps':12345}})
        expected=call('pricing.get.admin.settings.pricing')['overrides']
        path=ROOT/'.build/plugin-platform/pricing-runtime/status.json'
        state=json.loads(path.read_text());state['nextRun']=int(time.time())-1
        temporary=path.with_suffix('.test-tmp');temporary.write_text(json.dumps(state));temporary.replace(path)
        started=time.time()
        for _ in range(60):
            state=json.loads(path.read_text())
            if state.get('lastFinished',0)>=int(started) and state.get('status') in {'succeeded','failed'}:break
            time.sleep(2)
        assert state.get('status')=='succeeded',state
        result=call('pricing.get.admin.settings.pricing')
        assert result['overrides']==expected
        count=sum(len(value) for value in result['synced'].values())
        assert count>0
        receipt={'dailyTaskExecuted':True,'manualOverridesPreserved':True,'syncedModels':count,'status':state,'production':False}
        (ROOT/'.build/plugin-stage/pricing-verification.json').write_text(json.dumps(receipt,indent=2),encoding='utf-8')
        print(json.dumps(receipt))
    finally:
        change={'action':'replace','pricing':original} if original else {'action':'reset'}
        call('pricing.post.admin.settings.pricing.update',{'provider':'openai','models':['gpt-5'],'change':change})

if __name__=='__main__':main()

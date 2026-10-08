"""诊断规则属于插件；凭据、出口和上游传输由网关能力提供。"""
import json
import sys


def call(data):
    print(json.dumps({'call':'diagnostic.observe','input':data}),flush=True)
    response=json.loads(sys.stdin.readline())
    if not response['ok']:
        raise ValueError('diagnostic unavailable')
    return response['value']


def run(data, observe=call):
    account=data['accountId']
    model=data.get('model','gpt-6-astra')
    effort=data.get('effort','low')
    if data.get('mode')=='text':
        return observe({'accountId':account,'input':{'phase':'text','model':model,'effort':effort,'prompt':data['prompt']}})
    if data.get('mode')!='quick':
        raise ValueError('invalid mode')
    prepared=observe({'accountId':account,'input':{'phase':'prepare','model':model,'effort':effort}})
    if not prepared.get('prepared') or not prepared.get('materialRef'):
        return {**prepared,'classification':None,'reason':'未获得配套材料，首次获取不判定'}
    reference=prepared['materialRef']
    result=observe({'accountId':account,'input':{'phase':'probe','model':model,'effort':effort,'materialRef':reference}})
    # 标签只属于发送的材料。新 state 未经重放不能标注。
    valid=result.get('status')==200 and result.get('sentMaterialRef')==reference and not result.get('failed')
    return {key:value for key,value in {**result,'classification':('29' if result.get('stateReturned') else '21') if valid else None,'bootstrapPerformed':bool(prepared.get('bootstrap'))}.items() if key not in ('materialRef','sentMaterialRef')}


if __name__=='__main__':
    print(json.dumps({'result':run(json.loads(sys.stdin.readline()))},ensure_ascii=False),flush=True)

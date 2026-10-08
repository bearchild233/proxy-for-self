"""Small real-API smoke test for the loopback staging environment only."""
import http.cookiejar
import json
from urllib.error import HTTPError
from urllib.request import HTTPCookieProcessor, Request, build_opener
import uuid

BASE = 'http://127.0.0.1:18335'


def main():
    client = build_opener(HTTPCookieProcessor(http.cookiejar.CookieJar()))

    def request(path, data=None, expected=200, origin=BASE):
        req = Request(BASE + path, data=None if data is None else json.dumps(data).encode(),
                      headers={'Origin': origin, 'Content-Type': 'application/json'})
        try:
            response = client.open(req, timeout=15)
        except HTTPError as error:
            response = error
        with response:
            value = json.loads(response.read())
            assert (200 <= response.status < 300 if expected == 200 else response.status == expected), (path, response.status, value)
            if expected == 200:
                assert value.get('code', 200) == 200 if isinstance(value, dict) else True, value
            return value.get('data', value) if isinstance(value, dict) else value

    request('/healthz')
    request('/api/plugin-platform/catalog', expected=403)
    request('/api/plugin-platform/local-login', {}, expected=403, origin='https://outside.invalid')
    request('/api/plugin-platform/local-login', {})
    catalog = request('/api/plugin-platform/catalog')
    assert len(catalog) == 12, len(catalog)
    plugin = next(item for item in catalog if item['id']=='groups')
    for item in catalog:
        for page in item['pages']:
            request('/api/plugin-platform/page/'+item['id']+'/'+item['contentVersion']+'/'+page['id'])
    prefix = '/api/plugin-platform/call/groups/' + plugin['contentVersion'] + '/'
    page = request('/api/plugin-platform/page/groups/' + plugin['contentVersion'] + '/main')
    assert ('export function create(runtime,bridge)' if plugin['pages'][0].get('runtime')=='vue-component' else '<div id="app">') in page['html']
    request(prefix + 'secrets.read', {}, expected=403)
    request(prefix + 'display-order.save', {'scope': 'accounts'}, expected=403)
    suffix = uuid.uuid4().hex[:6]
    ids = []
    for name in ['本机测试 A ', '本机测试 B ']:
        created = request(prefix + 'account-groups.post.admin.account-groups.create', {'name': name + suffix, 'description': '独立数据库中的测试分组', 'color': '#6366f1ff', 'disableFast': False})
        ids.append(created['id'])
    request(prefix + 'account-groups.post.admin.account-groups.update', {'id': ids[0], 'name': '本机测试 A ' + suffix,
                                     'description': '保存与独立更新验证', 'color': '#6366f1ff', 'disableFast': True})
    request(prefix + 'account-groups.post.admin.account-groups.disable', {'id': ids[1]})
    disabled = request(prefix + 'account-groups.get.admin.account-groups', {'page': 1, 'pageSize': 20, 'enabled': False})
    assert ids[1] in {row['id'] for row in disabled['items']}
    request(prefix + 'account-groups.post.admin.account-groups.enable', {'id': ids[1]})
    rows = request(prefix + 'account-groups.get.admin.account-groups', {'page': 1, 'pageSize': 100})['items']
    before = [row['id'] for row in rows]
    after = list(reversed(before))
    request(prefix + 'display-order.save', {'scope': 'groups', 'originalIds': before, 'orderedIds': after})
    assert [row['id'] for row in request(prefix + 'account-groups.get.admin.account-groups', {'page': 1, 'pageSize': 100})['items']] == after
    # Keep two examples for the user's preview; delete only the extra disposable record.
    extra = request(prefix + 'account-groups.post.admin.account-groups.create', {'name': '临时清理 ' + suffix, 'description': None, 'color': '#6366f1ff'})
    request(prefix + 'account-groups.post.admin.account-groups.delete', {'id': extra['id']})
    print(json.dumps({'health': True, 'authentication': True, 'origin_guard': True,
                      'capability_guard': True, 'create_update_delete': True, 'boolean_filter': True,
                      'persistent_order': True, 'plugin': plugin['id'], 'version': plugin['contentVersion'],
                      'production': False}))


if __name__ == '__main__':
    main()

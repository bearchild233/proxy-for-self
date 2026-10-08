import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { createMemoryHistory, createRouter } from 'vue-router'
import { installHostNavigation } from '../../packages/plugin-sdk/navigation.ts'

test('same-package settings routes and other-plugin routes reuse host pages', async () => {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: ['/settings', '/settings/access', '/settings/upstream'].map(
      path => ({ path, component: {} }),
    ),
  })
  const navigated = []
  installHostNavigation(router, '/settings', async (path) => {
    navigated.push(path)
  })
  await router.push('/settings')
  for (const path of [
    '/settings/access',
    '/settings/upstream',
    '/settings/pricing',
    '/settings/backup',
  ]) {
    await router.push(path)
    assert.equal(router.currentRoute.value.path, '/settings')
  }
  assert.deepEqual(navigated, [
    '/settings/access',
    '/settings/upstream',
    '/settings/pricing',
    '/settings/backup',
  ])
  await router.push('/settings?section=test')
  assert.equal(router.currentRoute.value.query.section, 'test')
  assert.equal(navigated.length, 4)
})
test('preloaded subpage initializes without redirecting to another frame', async () => {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/settings/access', component: {} }],
  })
  installHostNavigation(router, '/settings/access', async () => {
    assert.fail('Initial page must stay local')
  })
  await router.push('/settings/access')
  assert.equal(router.currentRoute.value.path, '/settings/access')
})

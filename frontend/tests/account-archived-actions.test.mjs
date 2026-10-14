/* eslint-disable no-new-func -- 与其他账号测试一致：剥离类型后在内存中执行真实源码 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'
import * as vue from 'vue'

function loadFunctions(path, dependencies, names) {
  const code = stripTypeScriptTypes(readFileSync(new URL(path, import.meta.url), 'utf8'), { mode: 'strip' })
    .replace(/^import .*$/gm, '')
    .replace(/^export /gm, '')
  return new Function(...Object.keys(dependencies), `${code}\nreturn { ${names.join(',')} }`)(...Object.values(dependencies))
}

function fixture(failure) {
  const calls = []
  const messages = []
  const { useAccountLifecycle } = loadFunctions('../../plugins/accounts/ui/page/composables/useAccountLifecycle.ts', {
    ref: vue.ref,
    batchUpdateAccounts: async (input) => {
      calls.push(input)
      if (failure)
        throw failure
      return { accountIds: input.accountIds, configRevision: 1 }
    },
    toast: { success: message => messages.push(message) },
  }, ['useAccountLifecycle'])
  const selectedIds = vue.ref(new Set(['acct_a', 'acct_b']))
  const reloads = []
  const lifecycle = useAccountLifecycle({ selectedIds, reload: async () => reloads.push(true) })
  return { lifecycle, selectedIds, calls, messages, reloads }
}

test('restoring one archived account calls batch-update, drops the selection and reloads', async () => {
  const { lifecycle, selectedIds, calls, messages, reloads } = fixture()
  await lifecycle.update({ id: 'acct_a' }, { restoreArchived: true })
  assert.deepEqual(calls, [{ accountIds: ['acct_a'], restoreArchived: true }])
  assert.deepEqual([...selectedIds.value], ['acct_b'])
  assert.equal(reloads.length, 1)
  assert.equal(lifecycle.saving.value.size, 0)
  assert.match(messages[0], /已恢复到主列表/)
})

test('batch restore sends every selected id once and clears the selection', async () => {
  const { lifecycle, selectedIds, calls } = fixture()
  await lifecycle.restoreSelected()
  assert.deepEqual(calls, [{ accountIds: ['acct_a', 'acct_b'], restoreArchived: true }])
  assert.equal(selectedIds.value.size, 0)
  assert.equal(lifecycle.batchRestoring.value, false)
})

// 处理器作为 @click 使用：rejected Promise 会冒泡到宿主 onErrorCaptured，把整页替换成“插件运行失败”。
test('failed lifecycle requests never reject out of the click handlers', async () => {
  const { lifecycle, selectedIds, reloads } = fixture(new Error('服务暂时不可用'))
  await assert.doesNotReject(lifecycle.update({ id: 'acct_a' }, { restoreArchived: true }))
  await assert.doesNotReject(lifecycle.restoreSelected())
  assert.equal(lifecycle.saving.value.size, 0)
  assert.equal(lifecycle.batchRestoring.value, false)
  assert.equal(selectedIds.value.size, 2, 'selection is kept so the restore can be retried')
  assert.equal(reloads.length, 0)
})

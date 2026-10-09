/* eslint-disable no-new-func -- 执行实际筛选组件，注入空目录以回归历史账号查询 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库现有逻辑测试共用内置 runner
import test from 'node:test'
import * as Vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import { renderToString } from 'vue/server-renderer'

const filename = '../src/components/CatalogFilter.vue'
const descriptor = parse(readFileSync(new URL(filename, import.meta.url), 'utf8'), { filename }).descriptor
const script = compileScript(descriptor, { id: 'quota-filter' })
const code = stripTypeScriptTypes(script.content, { mode: 'strip' })
  .replace(/import \{([^}]+)\} from ["']vue["']/g, (_, names) => `const {${names.replace(/\bas\b/g, ':')}} = Vue`)
  .replace(/^import .*$/gm, '')
  .replace('export default', 'return')

async function fixture() {
  const requests = []
  const getAccounts = async () => {
    requests.push('accounts')
    return { items: [], page: { totalPages: 1 } }
  }
  const getApiKeys = async () => ({ items: [{ id: 'key_a', name: 'Current key' }] })
  const component = new Function('Vue', 'getAccounts', 'getApiKeys', 'BaseButton', 'BaseInput', 'BaseSelect', 'getAccountGroups', 'getPricing', code)(Vue, getAccounts, getApiKeys)
  let state
  await renderToString(Vue.createSSRApp({
    ...component,
    setup(props, context) {
      state = component.setup(props, context)
      return () => null
    },
  }, {
    kind: 'quota',
    label: '按密钥或账号筛选',
    modelValue: 'key_a',
    supplementalOptions: {
      accounts: [{ label: 'Archived account', value: 'acct_a' }, { label: 'Another account', value: 'acct_b' }],
      keys: [{ label: 'Historical key', value: 'key_a' }],
    },
  }))
  return { state, requests }
}

test('quota switches to account lookup, clears the key and offers historical accounts without credentials', async () => {
  const { state, requests } = await fixture()
  assert.equal(state.category.value, 'keys')
  state.changeCategory('accounts')
  await Vue.nextTick()
  assert.equal(state.model.value, '')
  assert.equal(state.activeKind.value, 'accounts')
  assert.deepEqual(requests, ['accounts'])
  assert.deepEqual(state.options.value.map(option => option.value), ['', 'acct_a', 'acct_b'])
  state.model.value = 'acct_a'
  assert.equal(state.options.value.some(option => option.value === 'acct_b'), true)
  state.changeCategory('keys')
  await Vue.nextTick()
  assert.equal(state.model.value, '')
})

test('quota key lookup uses stable refs and current catalog names take priority over snapshots', async () => {
  const { state } = await fixture()
  await state.load()
  const keys = state.options.value.filter(option => option.value)
  assert.deepEqual(keys, [{ label: 'Current key', description: undefined, value: 'key_a' }])
})

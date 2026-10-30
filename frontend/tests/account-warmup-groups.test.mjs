/* eslint-disable no-new-func, style/max-statements-per-line -- 执行真实 SFC/composable，验证分组范围和批量草稿边界 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 仓库统一使用 Node runner
import test from 'node:test'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import * as model from '../../plugins/account-warmup/ui/page/model.ts'

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8')
function evaluate(source, modules, exports) {
  const code = stripTypeScriptTypes(source, { mode: 'strip' })
    .replace(/import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]/g, (_, bindings, name) => `const {${bindings.replace(/\bas\b/g, ':')}} = modules[${JSON.stringify(name)}]`)
    .replace(/import\s+(\w+)\s+from\s+['"]([^'"]+)['"]/g, (_, binding, name) => `const ${binding} = modules[${JSON.stringify(name)}]`)
    .replace(/export (async function|function|const)/g, '$1')
    .replace('export default', 'return')
  return new Function('modules', code + (exports ? `\nreturn {${exports}}` : ''))({ vue, ...modules })
}
const helpers = evaluate(read('../src/api/modules/accountWarmup.ts'), { '@sdk/ui': {} }, 'warmupRequestError,warmupEffortOptions')
function sfc(path, modules) {
  const { descriptor } = parse(read(path))
  return evaluate(compileScript(descriptor, { id: 'warmup-groups-test', inlineTemplate: true }).content, modules)
}
function renderer() {
  const node = (type, text = '') => ({ type, text, props: {}, children: [], parent: null })
  const runtime = vue.createRenderer({
    createElement: type => node(type),
    createText: text => node('text', text),
    createComment: () => node('comment'),
    insert(child, parent, anchor) {
      if (child.parent)
        child.parent.children.splice(child.parent.children.indexOf(child), 1)
      child.parent = parent
      const at = anchor ? parent.children.indexOf(anchor) : -1
      parent.children.splice(at < 0 ? parent.children.length : at, 0, child)
    },
    remove(child) { child.parent?.children.splice(child.parent.children.indexOf(child), 1) },
    patchProp(node, key, _, value) { node.props[key] = value },
    setText(node, text) { node.text = text },
    setElementText(node, text) { node.text = text; node.children = [] },
    parentNode: node => node.parent,
    nextSibling: node => node.parent?.children[node.parent.children.indexOf(node) + 1] ?? null,
  })
  return { runtime, root: node('root') }
}
const all = (node, predicate) => [...(predicate(node) ? [node] : []), ...node.children.flatMap(child => all(child, predicate))]
const text = node => node.text + node.children.map(text).join('')
function field(root, label) {
  const result = all(root, node => node.props['aria-label'] === label)[0]
  assert.ok(result, `missing field: ${label}`)
  return result
}
function findButton(root, label) {
  const result = all(root, node => node.type === 'button' && text(node).includes(label))[0]
  assert.ok(result, `missing button: ${label}`)
  return result
}
async function settle() { for (let i = 0; i < 10; i++) await vue.nextTick() }
async function change(root, label, value) {
  const node = field(root, label)
  assert.ok(!node.props.disabled, `${label} is disabled`)
  const handler = node.type === 'select' || node.props.type === 'checkbox' ? node.props.onChange : node.props.onInput
  handler({ target: { value, checked: value } })
  await settle()
}
async function click(root, label) {
  const node = findButton(root, label)
  assert.ok(!node.props.disabled, `${label} is disabled`)
  await node.props.onClick()
  await settle()
}
const icon = () => vue.h('i')
const button = vue.defineComponent({ props: ['disabled', 'label'], setup: (props, { attrs, slots }) => () => vue.h('button', { ...attrs, 'disabled': props.disabled, 'aria-label': props.label }, slots.default?.()) })
const box = vue.defineComponent({ props: ['modelValue', 'label', 'disabled', 'indeterminate'], emits: ['update:modelValue'], setup: (props, { emit }) => () => vue.h('input', { 'type': 'checkbox', 'aria-label': props.label, 'checked': props.modelValue, 'disabled': props.disabled, 'indeterminate': props.indeterminate, 'onChange': e => emit('update:modelValue', e.target.checked) }) })
const input = vue.defineComponent({ props: ['modelValue', 'disabled'], emits: ['update:modelValue'], setup: (props, { attrs, emit }) => () => vue.h('input', { ...attrs, value: props.modelValue, disabled: props.disabled, onInput: e => emit('update:modelValue', e.target.value) }) })
const select = vue.defineComponent({ props: ['modelValue', 'options', 'disabled', 'label'], emits: ['update:modelValue'], setup: (props, { attrs, emit }) => () => vue.h('select', { ...attrs, 'aria-label': attrs['aria-label'] ?? props.label, 'value': props.modelValue, 'disabled': props.disabled, 'onChange': e => emit('update:modelValue', e.target.value) }, (props.options ?? []).map(option => vue.h('option', { value: option.value, disabled: option.disabled }, option.label))) })
const wrapper = vue.defineComponent({ props: ['title', 'label', 'description', 'error'], setup: (props, { slots }) => () => vue.h('div', [props.title, props.label, props.description, props.error, slots.actions?.(), slots.default?.()]) })
const modal = vue.defineComponent({ props: ['modelValue'], setup: (props, { slots }) => () => props.modelValue ? vue.h('section', [slots.default?.(), slots.footer?.()]) : null })
const base = {
  '@/components/base/BaseButton.vue': button,
  '@/components/base/BaseCard.vue': wrapper,
  '@/components/base/BaseCheckbox.vue': box,
  '@/components/base/BaseSwitch.vue': box,
  '@/components/base/BaseInput.vue': input,
  '@/components/base/BaseSelect.vue': select,
  '@/components/base/BaseSegmented.vue': select,
  '@/components/base/BaseForm/FormItem.vue': wrapper,
  '@/components/base/BaseModal/index.vue': modal,
}
const account = (number, groups = []) => ({ id: `account-${String(number).padStart(3, '0')}`, provider: 'openai', authenticationKind: 'oauth', enabled: true, status: 'normal', supported: true, reasonCode: null, modelAllowed: true, models: null, shortTerm: null, groups })
const group = (id, name = id, enabled = true) => ({ id, name, color: '', enabled })
const groupState = (groups = []) => ({ groups: vue.shallowRef(groups), loading: vue.shallowRef(false), error: vue.shallowRef(''), async refresh() {} })
function mountTargets(options = {}) {
  const settings = options.settings ?? vue.ref(model.defaultWarmupSettings())
  const groups = options.groups ?? groupState([group('odd'), group('even')])
  const props = vue.reactive({ accounts: Array.from({ length: 45 }, (_, index) => account(index + 1, [(index + 1) % 2 ? 'odd' : 'even'])), namesError: '', disabled: false, groupsSupported: true, models: {}, modelLoading: new Set(), modelErrors: {}, ...options.props })
  props.names = new Map(props.accounts.map(item => [item.id, { name: `Account ${item.id.slice(-3)}` }]))
  const row = sfc('../../plugins/account-warmup/ui/page/components/WarmupAccountRow.vue', { ...base, '@lucide/vue': { ChevronDown: icon, RefreshCw: icon }, '@/api/modules/accountWarmup': helpers, '../model': model })
  const batch = sfc('../../plugins/account-warmup/ui/page/components/WarmupBatchModal.vue', { ...base, '@/api/modules/accountWarmup': helpers })
  const component = sfc('../../plugins/account-warmup/ui/page/components/WarmupTargets.vue', {
    ...base,
    '@lucide/vue': { FolderTree: icon, ListChecks: icon, RefreshCw: icon },
    '../model': model,
    '../useWarmupGroups': { useWarmupGroups: () => groups },
    './WarmupAccountRow.vue': row,
    './WarmupBatchModal.vue': batch,
  })
  const loads = []
  const r = renderer()
  const app = r.runtime.createApp({ render: () => vue.h(component, { ...props, 'modelValue': settings.value, 'onUpdate:modelValue': value => settings.value = value, 'onModels': (...args) => loads.push(args) }) })
  app.mount(r.root)
  return { ...r, app, settings, groups, props, loads }
}
function mountGroups(getAccountGroups) {
  const { useWarmupGroups } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupGroups.ts'), { '@/api/modules/account-groups': { getAccountGroups } }, 'useWarmupGroups')
  const r = renderer()
  let state
  const app = r.runtime.createApp({ setup() { state = useWarmupGroups(); return () => vue.h('div') } })
  app.mount(r.root)
  return { ...r, app, state }
}
function mountSettings(snapshot, api = {}) {
  const saves = []
  const { useWarmupSettings } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupSettings.ts'), {
    '@sdk/polling': { usePluginPolling() {} },
    '@/api/modules/accounts': { getAccounts: async () => ({ items: [], page: { totalPages: 1 } }) },
    '@/api/modules/accountWarmup': { ...helpers, readAccountWarmup: async () => snapshot, accountWarmupHistory: async () => ({ runs: [], items: [] }), configureAccountWarmup: async (revision, value) => { saves.push({ revision, settings: value }); return { ...snapshot, revision: revision + 1, settings: value } }, ...api },
    '@/components/base/BaseToast': { toast: { success() {} } },
    './model': model,
  }, 'useWarmupSettings')
  const r = renderer()
  let state
  const app = r.runtime.createApp({ setup() { state = useWarmupSettings(); return () => vue.h('div') } })
  app.mount(r.root)
  return { ...r, app, state, saves }
}

test('target matching unions current group membership and deduplicates accounts without freezing selected ids', () => {
  const settings = model.defaultWarmupSettings()
  settings.targetMode = 'groups'
  settings.groupIds = ['a', 'b']
  const accounts = [account(1, ['a']), account(2, ['a', 'b']), account(3, ['b']), account(2, ['b']), account(4, ['c'])]
  assert.deepEqual(model.matchingWarmupAccounts(settings, accounts).map(item => item.id), ['account-001', 'account-002', 'account-003'])
  assert.deepEqual(settings.accountIds, [])
  assert.deepEqual(model.matchingWarmupAccounts(settings, [account(1, ['c']), account(2, ['b']), account(5, ['a'])]).map(item => item.id), ['account-002', 'account-005'])
  settings.targetMode = 'selected'
  settings.accountIds = ['account-004']
  assert.deepEqual(model.matchingWarmupAccounts(settings, accounts).map(item => item.id), ['account-004'])
  settings.targetMode = 'all'
  assert.equal(model.matchingWarmupAccounts(settings, accounts).length, 4)
})

test('missing group ids normalize to an independent empty array without mutating legacy settings', () => {
  const legacy = model.defaultWarmupSettings()
  delete legacy.groupIds
  const normalized = model.cloneSettings(legacy)
  assert.deepEqual(normalized.groupIds, [])
  normalized.groupIds.push('new')
  assert.equal(Object.hasOwn(legacy, 'groupIds'), false)
  assert.deepEqual(model.defaultWarmupSettings().groupIds, [])
})

test('old service settings and unknown dynamic group capability block saving while preserving the draft', async () => {
  const legacy = model.defaultWarmupSettings()
  delete legacy.groupIds
  let snapshot = { available: true, revision: 7, settings: legacy, accounts: [account(1)], runtime: { ready: true } }
  const f = mountSettings(snapshot, { readAccountWarmup: async () => snapshot })
  await settle()
  assert.deepEqual(f.state.draft.value.groupIds, [])
  f.state.draft.value.prompt = 'unsaved legacy edit'
  await f.state.save()
  assert.equal(f.saves.length, 0)
  assert.match(f.state.validation.value, /升级/)
  assert.equal(f.state.draft.value.prompt, 'unsaved legacy edit')
  snapshot = { ...snapshot, settings: { ...model.defaultWarmupSettings(), targetMode: 'groups', groupIds: ['saved-group'] } }
  await f.state.refresh(true)
  f.state.draft.value.prompt = 'unsaved group edit'
  await f.state.save()
  assert.equal(f.saves.length, 0)
  assert.match(f.state.validation.value, /尚未确认动态分组能力/)
  snapshot = { ...snapshot, groupTargetsSupported: true }
  await f.state.refresh()
  assert.equal(f.state.validation.value, '')
  assert.equal(f.state.draft.value.prompt, 'unsaved group edit')
  snapshot = { ...snapshot, groupTargetsSupported: false }
  await f.state.refresh()
  await f.state.save()
  assert.equal(f.saves.length, 0)
  assert.match(f.state.validation.value, /尚未确认动态分组能力/)
  assert.deepEqual(f.state.draft.value.groupIds, ['saved-group'])
  assert.equal(f.state.draft.value.prompt, 'unsaved group edit')
  f.app.unmount()
})

test('group metadata reads every page, deduplicates ids and preserves the complete old catalog after a page fails', async () => {
  const calls = []
  let failing = false
  const f = mountGroups(async (input, options) => {
    calls.push({ input, options })
    if (failing && input.page === 2)
      throw new Error('group page unavailable')
    return { items: input.page === 1 ? [group('a', failing ? 'Partial replacement' : 'A')] : [group('b'), group('a', 'Latest A')], page: { pageSize: 37, totalPages: 2 } }
  })
  await settle()
  assert.deepEqual(calls.map(item => item.input), [{ page: 1, pageSize: 200 }, { page: 2, pageSize: 37 }])
  assert.ok(calls.every(item => item.options.silent && item.options.signal instanceof AbortSignal))
  assert.deepEqual(f.state.groups.value.map(item => [item.id, item.name]), [['a', 'Latest A'], ['b', 'b']])
  const old = f.state.groups.value
  failing = true
  await f.state.refresh()
  assert.strictEqual(f.state.groups.value, old)
  assert.match(f.state.error.value, /group page unavailable/)
  assert.equal(f.state.loading.value, false)
  f.app.unmount()
  assert.equal(calls[0].options.signal.aborted, true)
})

test('newer metadata refresh wins and a disposed refresh cannot replace its retained catalog', async () => {
  const pending = []
  const f = mountGroups(() => new Promise(resolve => pending.push(resolve)))
  const newer = f.state.refresh()
  pending[1]({ items: [group('new')], page: { totalPages: 1 } })
  await newer
  pending[0]({ items: [group('old')], page: { totalPages: 1 } })
  await settle()
  assert.deepEqual(f.state.groups.value.map(item => item.id), ['new'])
  const last = f.state.refresh()
  f.app.unmount()
  pending[2]({ items: [group('disposed')], page: { totalPages: 1 } })
  await last
  assert.deepEqual(f.state.groups.value.map(item => item.id), ['new'])
})

test('selected account checkboxes survive page changes, name searches and group filters', async () => {
  const settings = vue.ref({ ...model.defaultWarmupSettings(), targetMode: 'selected', accountIds: ['saved-missing-account'] })
  const f = mountTargets({ settings })
  await change(f.root, '选择本页账号', true)
  assert.equal(settings.value.accountIds.length, 21)
  await click(f.root, '下一页')
  await change(f.root, '选择 Account 021', true)
  await change(f.root, '搜索预热账号', 'Account 045')
  await change(f.root, '选择 Account 045', true)
  await change(f.root, '筛选预热账号分组', 'even')
  assert.match(text(f.root), /暂无匹配账号/)
  assert.equal(settings.value.accountIds.length, 23)
  await change(f.root, '搜索预热账号', '')
  assert.equal(field(f.root, '选择 Account 002').props.checked, true)
  assert.ok(settings.value.accountIds.includes('account-021'))
  assert.ok(settings.value.accountIds.includes('account-045'))
  assert.ok(settings.value.accountIds.includes('saved-missing-account'))
  f.app.unmount()
})

test('all and group mode checkboxes manage overrides while their participating scope stays dynamic', async () => {
  const f = mountTargets({ props: { accounts: [account(1, ['a']), account(2, ['a', 'b']), account(3, ['b']), account(4)] }, groups: groupState([group('a', 'A', false), group('b', 'B')]) })
  await change(f.root, '选择 Account 001', true)
  assert.deepEqual(f.settings.value.accountIds, [])
  assert.equal(model.matchingWarmupAccounts(f.settings.value, f.props.accounts).length, 4)
  await change(f.root, '自动预热账号范围', 'groups')
  await change(f.root, '选择分组 A', true)
  await change(f.root, '选择分组 B', true)
  assert.match(text(f.root), /范围 3 个 · 勾选 0 个/)
  await click(f.root, '选中分组成员')
  assert.match(text(f.root), /范围 3 个 · 勾选 3 个/)
  assert.deepEqual(f.settings.value.accountIds, [])
  f.props.accounts = [account(1), account(2, ['b']), account(3, ['b']), account(5, ['a'])]
  await settle()
  assert.equal(all(f.root, node => node.props['aria-label'] === '选择 Account 001').length, 0)
  assert.ok(field(f.root, '选择 account-005'))
  assert.match(text(f.root), /范围 3 个 · 勾选 3 个/)
  assert.deepEqual(f.settings.value.groupIds, ['a', 'b'])
  await click(f.root, '批量管理')
  await change(f.root, '批量自动参与', 'disabled')
  await click(f.root, '应用到 2 个账号')
  assert.deepEqual(f.settings.value.accountOverrides.map(item => item.accountId), ['account-002', 'account-003'])
  assert.ok(f.settings.value.accountOverrides.every(item => !item.enabled))
  assert.equal(model.accountOverride(f.settings.value, 'account-001').enabled, true)
  assert.equal(model.accountOverride(f.settings.value, 'account-005').enabled, true)
  f.app.unmount()
})

test('unknown group capability disables adding groups and selected missing ids remain removable after metadata errors', async () => {
  const groups = groupState([group('known', 'Known')])
  groups.error.value = 'metadata temporarily unavailable'
  const settings = vue.ref({ ...model.defaultWarmupSettings(), targetMode: 'groups', groupIds: ['missing', 'known'] })
  const f = mountTargets({ settings, groups, props: { groupsSupported: false, accounts: [account(1, ['known'])] } })
  const option = all(field(f.root, '自动预热账号范围'), node => node.type === 'option' && node.props.value === 'groups')[0]
  assert.equal(option.props.disabled, true)
  assert.match(text(f.root), /metadata temporarily unavailable/)
  assert.equal(field(f.root, '选择分组 未知分组 · missing').props.disabled, false)
  await change(f.root, '选择分组 未知分组 · missing', false)
  await change(f.root, '选择分组 Known', false)
  assert.deepEqual(settings.value.groupIds, [])
  assert.equal(field(f.root, '选择分组 Known').props.disabled, true)
  f.app.unmount()
})

test('selection and group caps reject the whole attempted addition without losing prior choices', async () => {
  const accounts = Array.from({ length: 501 }, (_, index) => account(index + 1))
  const settings = vue.ref({ ...model.defaultWarmupSettings(), targetMode: 'selected', accountIds: ['account-001'] })
  const f = mountTargets({ settings, props: { accounts } })
  await click(f.root, '选中筛选结果')
  assert.deepEqual(settings.value.accountIds, ['account-001'])
  assert.match(text(f.root), /最多同时选择 500/)
  settings.value.targetMode = 'groups'
  settings.value.groupIds = Array.from({ length: 500 }, (_, index) => `saved-${index}`)
  f.groups.groups.value = [group('additional', 'Additional')]
  await settle()
  const before = [...settings.value.groupIds]
  await change(f.root, '选择分组 Additional', true)
  assert.deepEqual(settings.value.groupIds, before)
  assert.match(text(f.root), /最多选择 500 个分组/)
  f.app.unmount()
})

test('real account row model events forward both account id and effective model through targets', async () => {
  const settings = vue.ref({ ...model.defaultWarmupSettings(), accountOverrides: [{ accountId: 'account-001', enabled: true, dailyTime: null, model: 'independent-model', reasoningEffort: null, resetDelaySeconds: null }] })
  const f = mountTargets({ settings, props: { accounts: [account(1)] } })
  const expand = all(f.root, node => node.type === 'button' && node.props['aria-expanded'] === false)[0]
  expand.props.onClick()
  await settle()
  await click(f.root, '模型目录')
  assert.deepEqual(f.loads, [['account-001', 'independent-model'], ['account-001', 'independent-model']])
  f.app.unmount()
})

test('batch patches preserve all unchosen fields, inherit with null and reject invalid or oversized updates atomically', () => {
  const settings = model.defaultWarmupSettings()
  settings.accountOverrides = [{ accountId: 'a', enabled: false, dailyTime: '12:34', model: 'custom-a', reasoningEffort: 'high', resetDelaySeconds: 99 }]
  const original = JSON.stringify(settings)
  const changed = model.updateWarmupOverrides(settings, ['a', 'b', 'a'], { model: 'batch-model' })
  assert.deepEqual(changed.accountOverrides[0], { ...settings.accountOverrides[0], model: 'batch-model' })
  assert.deepEqual(changed.accountOverrides[1], { ...model.accountOverride(settings, 'b'), model: 'batch-model' })
  assert.equal(JSON.stringify(settings), original)
  const inherited = model.updateWarmupOverrides(changed, ['a'], { dailyTime: null, model: null, reasoningEffort: null, resetDelaySeconds: null })
  assert.deepEqual(inherited.accountOverrides[0], { accountId: 'a', enabled: false, dailyTime: null, model: null, reasoningEffort: null, resetDelaySeconds: null })
  for (const patch of [{ dailyTime: '24:00' }, { model: ' ' }, { model: '中'.repeat(43) }, { model: 'a\nb' }, { model: 'a\u007Fb' }, { reasoningEffort: 'unsupported' }, { resetDelaySeconds: -1 }, { resetDelaySeconds: 86401 }, { resetDelaySeconds: Number.NaN }]) {
    assert.throws(() => model.updateWarmupOverrides(settings, ['a', 'b'], patch))
    assert.equal(JSON.stringify(settings), original)
  }
  assert.throws(() => model.updateWarmupOverrides(settings, Array.from({ length: 501 }, (_, index) => `account-${index}`), { enabled: true }), /500/)
  const full = model.defaultWarmupSettings()
  full.accountOverrides = Array.from({ length: 500 }, (_, index) => model.accountOverride(full, `existing-${index}`))
  const fullBefore = JSON.stringify(full)
  assert.throws(() => model.updateWarmupOverrides(full, ['existing-0', 'new'], { enabled: false }), /500/)
  assert.equal(JSON.stringify(full), fullBefore)
  assert.equal(model.updateWarmupOverrides(full, ['existing-0'], { enabled: false }).accountOverrides[0].enabled, false)
  settings.dailyTime = 'unfinished global edit'
  assert.equal(model.updateWarmupOverrides(settings, ['a'], { enabled: true }).accountOverrides[0].enabled, true)
})

test('real batch modal defaults to preserve, applies only chosen fields and supports inheriting every override', async () => {
  const settings = vue.ref(model.defaultWarmupSettings())
  settings.value.accountOverrides = [{ accountId: 'account-001', enabled: false, dailyTime: '12:34', model: 'custom', reasoningEffort: 'high', resetDelaySeconds: 99 }]
  const f = mountTargets({ settings, props: { accounts: [account(1), account(2)] } })
  await click(f.root, '选中筛选结果')
  await click(f.root, '批量管理')
  for (const label of ['批量自动参与', '批量时间模式', '批量模型模式', '批量强度模式', '批量延迟模式'])
    assert.equal(field(f.root, label).props.value, 'keep')
  assert.equal(findButton(f.root, '应用到 2 个账号').props.disabled, true)
  await change(f.root, '批量模型模式', 'custom')
  await change(f.root, '批量模型 ID', 'batch-model')
  await click(f.root, '应用到 2 个账号')
  assert.deepEqual(settings.value.accountOverrides[0], { accountId: 'account-001', enabled: false, dailyTime: '12:34', model: 'batch-model', reasoningEffort: 'high', resetDelaySeconds: 99 })
  assert.deepEqual(settings.value.accountOverrides[1], { ...model.accountOverride(model.defaultWarmupSettings(), 'account-002'), model: 'batch-model' })
  await click(f.root, '批量管理')
  assert.equal(field(f.root, '批量模型模式').props.value, 'keep')
  for (const label of ['批量时间模式', '批量模型模式', '批量强度模式', '批量延迟模式'])
    await change(f.root, label, 'inherit')
  await click(f.root, '应用到 2 个账号')
  assert.deepEqual(settings.value.accountOverrides[0], { accountId: 'account-001', enabled: false, dailyTime: null, model: null, reasoningEffort: null, resetDelaySeconds: null })
  assert.deepEqual(settings.value.accountIds, [])
  assert.equal(settings.value.targetMode, 'all')
  f.app.unmount()
})

test('real batch modal blocks invalid inputs and cap errors keep every original override intact', async () => {
  const settings = vue.ref(model.defaultWarmupSettings())
  settings.value.accountOverrides = Array.from({ length: 500 }, (_, index) => model.accountOverride(settings.value, `existing-${index}`))
  const f = mountTargets({ settings, props: { accounts: [account(1)] } })
  await change(f.root, '选择 Account 001', true)
  await click(f.root, '批量管理')
  await change(f.root, '批量时间模式', 'custom')
  await change(f.root, '批量每日时间', '25:00')
  assert.equal(findButton(f.root, '应用到 1 个账号').props.disabled, true)
  assert.match(text(f.root), /HH:mm/)
  await change(f.root, '批量时间模式', 'keep')
  await change(f.root, '批量模型模式', 'custom')
  await change(f.root, '批量模型 ID', ' ')
  assert.equal(findButton(f.root, '应用到 1 个账号').props.disabled, true)
  await change(f.root, '批量模型模式', 'keep')
  await change(f.root, '批量延迟模式', 'custom')
  await change(f.root, '批量重置延迟', '')
  assert.equal(findButton(f.root, '应用到 1 个账号').props.disabled, true)
  await change(f.root, '批量重置延迟', '86401')
  assert.equal(findButton(f.root, '应用到 1 个账号').props.disabled, true)
  await change(f.root, '批量延迟模式', 'keep')
  await change(f.root, '批量自动参与', 'disabled')
  const before = JSON.stringify(settings.value)
  await click(f.root, '应用到 1 个账号')
  assert.equal(JSON.stringify(settings.value), before)
  assert.match(text(f.root), /账号独立设置最多保留 500/)
  assert.ok(field(f.root, '批量自动参与'))
  f.app.unmount()
})

test('batch application only updates the shared settings draft and configure runs after explicit save', async () => {
  const settings = model.defaultWarmupSettings()
  const saved = mountSettings({ available: true, revision: 7, settings, accounts: [account(1)], runtime: { ready: true } })
  const { state } = saved
  await settle()
  const f = mountTargets({ settings: state.draft, props: { accounts: [account(1)] } })
  await change(f.root, '选择 Account 001', true)
  await click(f.root, '批量管理')
  await change(f.root, '批量自动参与', 'disabled')
  await click(f.root, '应用到 1 个账号')
  assert.equal(state.dirty.value, true)
  assert.equal(saved.saves.length, 0)
  assert.equal(model.accountOverride(state.draft.value, 'account-001').enabled, false)
  assert.deepEqual(settings.accountOverrides, [])
  await state.save()
  assert.equal(saved.saves.length, 1)
  assert.equal(saved.saves[0].revision, 7)
  assert.equal(state.dirty.value, false)
  f.app.unmount()
  saved.app.unmount()
})

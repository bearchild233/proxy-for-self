/* eslint-disable no-new-func, style/max-statements-per-line -- 执行真实 SFC 和 composable，验证自动化配置与来源边界 */
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
  return evaluate(compileScript(descriptor, { id: 'automation-test', inlineTemplate: true }).content, modules)
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
const field = (root, label) => all(root, n => n.props['aria-label'] === label)[0]
async function settle() { for (let i = 0; i < 10; i++) await vue.nextTick() }
const icon = () => vue.h('i')
const button = vue.defineComponent({ props: ['disabled', 'label'], setup: (props, { attrs, slots }) => () => vue.h('button', { ...attrs, 'disabled': props.disabled, 'aria-label': props.label }, slots.default?.()) })
const box = vue.defineComponent({ props: ['modelValue', 'label', 'disabled'], emits: ['update:modelValue'], setup: (props, { emit }) => () => vue.h('input', { 'type': 'checkbox', 'aria-label': props.label, 'checked': props.modelValue, 'disabled': props.disabled, 'onChange': e => emit('update:modelValue', e.target.checked) }) })
const input = vue.defineComponent({ props: ['modelValue', 'disabled'], emits: ['update:modelValue'], setup: (props, { attrs, emit }) => () => vue.h('input', { ...attrs, value: props.modelValue, disabled: props.disabled, onInput: e => emit('update:modelValue', e.target.value) }) })
const select = vue.defineComponent({ props: ['modelValue', 'options', 'disabled'], emits: ['update:modelValue'], setup: (props, { attrs, emit }) => () => vue.h('select', { ...attrs, value: props.modelValue, disabled: props.disabled, onChange: e => emit('update:modelValue', e.target.value) }, (props.options ?? []).map(option => vue.h('option', { value: option.value, disabled: option.disabled }, option.label))) })
const wrapper = vue.defineComponent({ props: ['title', 'label', 'description', 'error'], setup: (props, { slots }) => () => vue.h('div', [props.title, props.label, props.description, props.error, slots.actions?.(), slots.default?.()]) })
const base = {
  '@/components/base/BaseButton.vue': button,
  '@/components/base/BaseCard.vue': wrapper,
  '@/components/base/BasePageHeader.vue': wrapper,
  '@/components/base/BaseCheckbox.vue': box,
  '@/components/base/BaseSwitch.vue': box,
  '@/components/base/BaseInput.vue': input,
  '@/components/base/BaseTextarea.vue': input,
  '@/components/base/BaseSelect.vue': select,
  '@/components/base/BaseSegmented.vue': select,
  '@/components/base/BaseForm/FormItem.vue': wrapper,
}
const account = { id: 'account-a', provider: 'openai', authenticationKind: 'oauth', enabled: true, status: 'normal', supported: true, reasonCode: null, modelAllowed: true, models: null, shortTerm: null }
const run = { id: 'auto-daily', source: 'daily', status: 'queued', createdAt: '2026-10-30T00:00:00Z', finishedAt: null, total: 1, accepted: 0, failed: 0, skipped: 0, unknown: 0, pending: 1 }
const snapshot = () => ({ available: true, revision: 3, settings: model.defaultWarmupSettings(), accounts: [account], runtime: { ready: true, nextRunAt: null, lastRun: null } })
function mountSettings(api = {}) {
  const { useWarmupSettings } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupSettings.ts'), {
    '@sdk/polling': { usePluginPolling() {} },
    '@/api/modules/accounts': { getAccounts: async () => ({ items: [], page: { totalPages: 1 } }) },
    '@/api/modules/accountWarmup': {
      ...helpers,
      readAccountWarmup: async () => snapshot(),
      accountWarmupHistory: async () => ({ runs: [], items: [] }),
      cancelAccountWarmup: async () => ({ run }),
      configureAccountWarmup: async (_, settings) => ({ ...snapshot(), revision: 4, settings }),
      ...api,
    },
    '@/components/base/BaseToast': { toast: { success() {} } },
    './model': model,
  }, 'useWarmupSettings')
  const r = renderer()
  let state
  const app = r.runtime.createApp({ setup() { state = useWarmupSettings(); return () => vue.h('div') } })
  app.mount(r.root)
  return { ...r, app, state }
}

test('legacy settings normalize request fields before dirty tracking and save retains prompt bytes', async () => {
  const legacy = snapshot()
  delete legacy.settings.prompt
  delete legacy.settings.reasoningEffort
  legacy.settings.accountOverrides = [{ accountId: account.id, enabled: true, dailyTime: '11:20', model: null, resetDelaySeconds: 19 }]
  const saved = []
  const f = mountSettings({
    readAccountWarmup: async () => legacy,
    configureAccountWarmup: async (revision, settings) => { saved.push({ revision, settings }); return { ...snapshot(), revision: 4, settings } },
  })
  await settle()
  assert.equal(f.state.draft.value.prompt, 'hi')
  assert.equal(f.state.draft.value.reasoningEffort, 'auto')
  assert.equal(f.state.draft.value.accountOverrides[0].reasoningEffort, null)
  assert.equal(f.state.dirty.value, false)
  f.state.draft.value.prompt = '  hello\n\tworld\r\n '
  f.state.draft.value.reasoningEffort = 'high'
  await f.state.save()
  assert.equal(saved[0].revision, 3)
  assert.equal(saved[0].settings.prompt, '  hello\n\tworld\r\n ')
  assert.equal(saved[0].settings.reasoningEffort, 'high')
  assert.equal(saved[0].settings.accountOverrides[0].dailyTime, '11:20')
  assert.equal(f.state.dirty.value, false)
  assert.equal(Object.hasOwn(legacy.settings, 'prompt'), false)
  f.app.unmount()
})

test('automation save validates UTF-8 bytes, permitted whitespace and independent efforts', async () => {
  let saves = 0
  const f = mountSettings({ configureAccountWarmup: async (_, settings) => { saves++; return { ...snapshot(), settings } } })
  await settle()
  f.state.draft.value.prompt = `${'中'.repeat(1365)}a`
  assert.equal(new TextEncoder().encode(f.state.draft.value.prompt).length, 4096)
  assert.equal(f.state.validation.value, '')
  f.state.draft.value.prompt += 'a'
  assert.match(f.state.validation.value, /4096/)
  await f.state.save()
  assert.equal(saves, 0)
  for (const prompt of [' \n\r\t ', 'a\u0000b', 'a\u007Fb', 'a\u0085b']) {
    f.state.draft.value.prompt = prompt
    assert.notEqual(f.state.validation.value, '')
  }
  f.state.draft.value.prompt = ' hi\n\r\t '
  assert.equal(f.state.validation.value, '')
  f.state.draft.value.accountOverrides = [{ ...model.accountOverride(f.state.draft.value, account.id), reasoningEffort: 'unsupported' }]
  assert.match(f.state.validation.value, /独立设置.*强度/)
  f.state.draft.value.accountOverrides[0].reasoningEffort = null
  assert.equal(f.state.validation.value, '')
  f.app.unmount()
})

test('real account rows keep effort overrides independent and inherit global effort when cleared', async () => {
  const settings = vue.reactive(model.defaultWarmupSettings())
  settings.reasoningEffort = 'medium'
  settings.accountOverrides = [{ accountId: account.id, enabled: true, dailyTime: '10:42', model: 'model-a', resetDelaySeconds: 37, reasoningEffort: null }]
  const component = sfc('../../plugins/account-warmup/ui/page/components/WarmupAccountRow.vue', { ...base, '@lucide/vue': { ChevronDown: icon, RefreshCw: icon }, '@/api/modules/accountWarmup': helpers, '../model': model })
  const r = renderer()
  const app = r.runtime.createApp({ render: () => vue.h('div', ['a', 'b'].map((name) => {
    const id = `account-${name}`
    return vue.h(component, { account: { ...account, id }, name, settings, selected: true, disabled: false, models: [{ id: name === 'a' ? 'model-a' : settings.defaultModel, label: name, reasoningEfforts: name === 'a' ? ['medium', 'high'] : ['low', 'medium'] }], modelsLoading: false, modelsError: '', onOverride: (value) => { settings.accountOverrides = [...settings.accountOverrides.filter(item => item.accountId !== id), value] } })
  })) })
  app.mount(r.root)
  for (const node of all(r.root, n => n.type === 'button' && n.props['aria-expanded'] === false))
    node.props.onClick()
  await settle()
  field(r.root, 'a 使用独立推理强度').props.onChange({ target: { checked: true } })
  field(r.root, 'b 使用独立推理强度').props.onChange({ target: { checked: true } })
  await settle()
  field(r.root, 'a 独立推理强度').props.onChange({ target: { value: 'high' } })
  field(r.root, 'b 独立推理强度').props.onChange({ target: { value: 'low' } })
  await settle()
  assert.equal(model.accountOverride(settings, 'account-a').reasoningEffort, 'high')
  assert.equal(model.accountOverride(settings, 'account-b').reasoningEffort, 'low')
  assert.equal(model.accountOverride(settings, 'account-a').dailyTime, '10:42')
  assert.equal(model.accountOverride(settings, 'account-a').resetDelaySeconds, 37)
  assert.equal(all(field(r.root, 'b 独立推理强度'), n => n.type === 'option' && n.props.value === 'high').length, 0)
  field(r.root, 'b 独立推理强度').props.onChange({ target: { value: 'high' } })
  assert.equal(model.accountOverride(settings, 'account-b').reasoningEffort, 'low')
  field(r.root, 'a 使用独立推理强度').props.onChange({ target: { checked: false } })
  await settle()
  assert.equal(model.accountOverride(settings, 'account-a').reasoningEffort, null)
  assert.equal(model.accountOverride(settings, 'account-b').reasoningEffort, 'low')
  assert.equal(settings.reasoningEffort, 'medium')
  app.unmount()
})

test('automatic history retains merged manual tasks but blocks manual run selection or cancellation', async () => {
  const calls = []
  const cancelled = []
  const manual = { ...run, id: 'manual', source: 'manual' }
  const automaticTask = { id: 'daily-task', accountId: account.id, source: 'daily' }
  const f = mountSettings({
    accountWarmupHistory: async (input) => {
      calls.push(input)
      return input.runId ? { runs: [run], items: [{ ...automaticTask, id: 'manual-task', source: 'manual' }, automaticTask] } : { runs: [manual, run], items: [] }
    },
    cancelAccountWarmup: async (id) => { cancelled.push(id); return { run } },
  })
  await settle()
  assert.deepEqual(calls[0], { limit: 20, source: 'automatic' })
  assert.deepEqual(calls[1], { runId: run.id, source: 'automatic' })
  assert.deepEqual(f.state.runs.value.map(item => item.id), [run.id])
  assert.deepEqual(f.state.items.value.map(item => item.id), ['manual-task', 'daily-task'])
  f.state.selectRun(manual.id)
  assert.equal(f.state.selectedRunId.value, run.id)
  f.state.selectedRunId.value = manual.id
  await f.state.cancel()
  assert.deepEqual(cancelled, [])
  f.state.selectedRunId.value = run.id
  await f.state.cancel()
  assert.deepEqual(cancelled, [run.id])
  f.app.unmount()
})

test('automatic history rejects details whose run is actually manual', async () => {
  const f = mountSettings({
    accountWarmupHistory: async input => input.runId
      ? { runs: [{ ...run, source: 'manual' }], items: [{ id: 'manual-task', accountId: account.id, source: 'manual' }] }
      : { runs: [run], items: [] },
  })
  await settle()
  assert.equal(f.state.selectedRunId.value, run.id)
  assert.deepEqual(f.state.items.value, [])
  assert.match(f.state.historyError.value, /批次来源不匹配/)
  f.app.unmount()
})

test('automation page edits the automatic request and reads an existing account model catalog', async () => {
  const loads = []
  const navigation = []
  const f = mountSettings()
  await settle()
  f.state.view.value.runtime.nextRunAt = '2035-01-01T00:00:00Z'
  f.state.view.value.runtime.nextAutomaticRunAt = '2037-01-01T00:00:00Z'
  f.state.models.value[account.id] = [{ id: 'catalog-model', label: 'Catalog model', reasoningEfforts: ['low', 'high'] }]
  const component = sfc('../../plugins/account-warmup/ui/page/index.vue', {
    ...base,
    '@lucide/vue': { ArrowLeft: icon, Clock3: icon, RefreshCw: icon, Save: icon },
    '@sdk/ui': { host: async (_, input) => navigation.push(input.path) },
    '@/api/modules/accountWarmup': helpers,
    './components/WarmupAccountRow.vue': wrapper,
    './components/WarmupHistory.vue': wrapper,
    './model': model,
    './useWarmupSettings': { useWarmupSettings: () => ({ ...f.state, loadModels: async (...args) => loads.push(args) }) },
  })
  const r = renderer()
  const app = r.runtime.createApp({ render: () => vue.h(component) })
  app.mount(r.root)
  await settle()
  assert.deepEqual(loads[0], [account.id, f.state.draft.value.defaultModel])
  assert.match(text(r.root), /自动预热请求/)
  assert.match(text(r.root), /不修改账号管理中手动预热的本次参数/)
  assert.ok(text(r.root).includes(model.formatWarmupTime('2037-01-01T00:00:00Z')))
  assert.ok(!text(r.root).includes(model.formatWarmupTime('2035-01-01T00:00:00Z')))
  assert.equal(all(r.root, n => n.type === 'option' && n.props.value === 'catalog-model').length, 1)
  field(r.root, '选择默认预热模型').props.onChange({ target: { value: 'catalog-model' } })
  field(r.root, '自动预热提示词').props.onInput({ target: { value: '你好\nworld' } })
  await settle()
  const efforts = field(r.root, '自动预热默认推理强度')
  assert.deepEqual(efforts.children.map(item => item.props.value), ['auto', 'low', 'high'])
  efforts.props.onChange({ target: { value: 'high' } })
  assert.equal(f.state.draft.value.reasoningEffort, 'high')
  assert.equal(f.state.draft.value.prompt, '你好\nworld')
  assert.match(text(r.root), /12 \/ 4096 字节/)
  await all(r.root, n => n.type === 'button' && text(n).includes('返回账号管理'))[0].props.onClick()
  assert.deepEqual(navigation, ['/accounts'])
  app.unmount()
  f.app.unmount()
})

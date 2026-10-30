/* eslint-disable no-new-func, style/max-statements-per-line -- 执行真实 Vue SFC 与紧凑内存 renderer，检查交互和异步竞态 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 仓库统一使用 Node runner
import test from 'node:test'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import { renderToString } from 'vue/server-renderer'
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
function sfc(path, modules) {
  const { descriptor } = parse(read(path))
  return evaluate(compileScript(descriptor, { id: 'warmup-test', inlineTemplate: true }).content, modules)
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
const icon = () => vue.h('i')
const button = vue.defineComponent({ props: ['disabled', 'label'], setup: (props, { attrs, slots }) => () => vue.h('button', { ...attrs, 'disabled': props.disabled, 'aria-label': props.label }, slots.default?.()) })
const box = vue.defineComponent({ props: ['modelValue', 'label', 'disabled'], emits: ['update:modelValue'], setup: (props, { emit }) => () => vue.h('input', { 'type': 'checkbox', 'aria-label': props.label, 'checked': props.modelValue, 'disabled': props.disabled, 'onChange': e => emit('update:modelValue', e.target.checked) }) })
const input = vue.defineComponent({ props: ['modelValue', 'disabled'], emits: ['update:modelValue'], setup: (props, { attrs, emit }) => () => vue.h('input', { ...attrs, value: props.modelValue, disabled: props.disabled, onInput: e => emit('update:modelValue', e.target.value) }) })
const wrapper = vue.defineComponent({ setup: (_, { slots }) => () => vue.h('div', {}, slots.default?.()) })
const base = {
  '@/components/base/BaseButton.vue': button,
  '@/components/base/BaseIconButton.vue': button,
  '@/components/base/BaseMenuItem.vue': button,
  '@/components/base/BaseCheckbox.vue': box,
  '@/components/base/BaseSwitch.vue': box,
  '@/components/base/BaseInput.vue': input,
  '@/components/base/BaseSelect.vue': input,
  '@/components/base/BaseForm/FormItem.vue': wrapper,
  '@/components/base/BaseTextarea.vue': input,
  '@/components/base/BaseModal/index.vue': vue.defineComponent({ props: ['modelValue'], setup: (props, { slots }) => () => props.modelValue ? vue.h('section', [slots.default?.(), slots.footer?.()]) : null }),
}
const requestHelpers = evaluate(read('../src/api/modules/accountWarmup.ts'), { '@sdk/ui': { call() {} } }, 'warmupRequestError,warmupEffortOptions')
const run = { id: 'run-1', source: 'manual', status: 'queued', createdAt: '2026-10-25T00:00:00Z', finishedAt: null, total: 2, accepted: 0, failed: 0, skipped: 0, unknown: 0, pending: 2 }
const account = { id: 'page-2-account', provider: 'openai', authenticationKind: 'oauth', enabled: true, status: 'normal', supported: true, reasonCode: null, modelAllowed: true, models: null, shortTerm: null }
const snapshot = (revision = 1) => ({ available: true, revision, settings: model.defaultWarmupSettings(), accounts: [account], accountStates: {}, runtime: { ready: true, nextRunAt: null, lastRun: null } })
async function settle() { for (let i = 0; i < 8; i++) await vue.nextTick() }

function mountToolbar(api, props = {}) {
  const modules = {
    ...base,
    '@lucide/vue': { ChevronDown: icon, Flame: icon, Settings2: icon },
    '@sdk/catalog': { enabled: () => true },
    '@sdk/polling': { usePluginPolling() {} },
    '@sdk/ui': { host: async () => true },
    '@/api/modules/accountWarmup': { ...requestHelpers, readAccountWarmup: async () => snapshot(), ...api },
    '@/components/base/BasePopover.vue': vue.defineComponent({ setup: (_, { slots }) => () => vue.h('div', [slots.trigger?.({ open: true }), slots.default?.({ close() {} })]) }),
  }
  const component = sfc('../../plugins/accounts/ui/page/components/AccountWarmupToolbar.vue', modules)
  const modal = sfc('../../plugins/accounts/ui/page/components/AccountWarmupModal.vue', modules)
  const { useAccountWarmup } = evaluate(read('../../plugins/accounts/ui/page/composables/useAccountWarmup.ts'), modules, 'useAccountWarmup')
  const r = renderer()
  const state = vue.reactive({ selectedAccountIds: ['page-1-account', 'page-2-account'], archived: false, ...props })
  let controller
  const app = r.runtime.createApp({ setup() {
    controller = vue.reactive(useAccountWarmup(vue.toRef(state, 'archived')))
    return () => vue.h('div', [
      vue.h(component, { ...state, ready: controller.ready, installed: controller.installed, submitting: controller.submitting, status: controller.status, onRun: ids => controller.open(ids) }),
      vue.h(modal, {
        'open': controller.dialogOpen,
        'onUpdate:open': value => controller.dialogOpen = value,
        'modelValue': controller.request,
        'onUpdate:modelValue': value => controller.request = value,
        'targetName': controller.targetName,
        'ready': controller.ready && !state.archived,
        'locked': controller.requestLocked,
        'unresolved': Boolean(controller.pendingSubmission),
        'submitting': controller.submitting,
        'error': controller.submitError,
        'models': controller.models,
        'accounts': controller.view?.available ? controller.view.accounts : [],
        'catalogAccountId': controller.modelAccountId,
        'modelsLoading': controller.modelsLoading,
        'modelsError': controller.modelsError,
        'onCatalog': controller.loadModels,
        'onSubmit': controller.submit,
      }),
    ])
  } })
  app.mount(r.root)
  return { ...r, app, state, controller }
}
test('real toolbar submits all cross-page selected IDs and an explicit all scope', async () => {
  const calls = []
  const f = mountToolbar({ runAccountWarmup: async (input) => { calls.push(input); return { run } } })
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('预热选中 (2)'))[0].props.onClick()
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('开始预热'))[0].props.onClick()
  await settle()
  assert.deepEqual(calls[0].accountIds, ['page-1-account', 'page-2-account'])
  assert.equal(calls[0].scope, 'selected')
  assert.match(text(f.root), /已加入预热队列/)
  assert.doesNotMatch(text(f.root), /预热成功/)
  await all(f.root, n => n.type === 'button' && text(n).includes('预热全部当前账号'))[0].props.onClick()
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('开始预热'))[0].props.onClick()
  await settle()
  assert.equal(calls[1].scope, 'all')
  assert.equal(Object.hasOwn(calls[1], 'accountIds'), false)
  f.app.unmount()
})
test('real toolbar retains submission ID after an unknown network result and blocks archived actions', async () => {
  const calls = []
  const f = mountToolbar({ runAccountWarmup: async (input) => {
    calls.push(input); if (calls.length === 1)
      throw new Error('网络断开'); return { run }
  } })
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('预热选中 (2)'))[0].props.onClick()
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('开始预热'))[0].props.onClick()
  await settle()
  assert.equal(f.controller.requestLocked, true)
  f.controller.dialogOpen = false
  f.controller.open(['a-different-account'])
  await settle()
  await all(f.root, n => n.type === 'button' && text(n).includes('核对提交结果'))[0].props.onClick()
  assert.equal(calls[0].submissionId, calls[1].submissionId)
  f.state.archived = true
  await settle()
  await f.controller.submit()
  assert.equal(calls.length, 2)
  assert.match(text(f.root), /归档账号不参与预热/)
  f.app.unmount()
})
test('unavailable service is an ordinary disabled toolbar state', async () => {
  const f = mountToolbar({ readAccountWarmup: async () => ({ available: false, reason: 'service_unavailable' }), runAccountWarmup: async () => assert.fail() })
  await settle()
  assert.match(text(f.root), /预热服务尚未就绪/)
  assert.equal(all(f.root, n => n.type === 'button' && text(n).includes('预热选中 (2)'))[0].props.disabled, true)
  f.app.unmount()
})
test('legacy services cannot submit new request fields and invalid model text is rejected before submission', async () => {
  const legacy = snapshot()
  delete legacy.settings.prompt
  delete legacy.settings.reasoningEffort
  delete legacy.accountStates
  const f = mountToolbar({ readAccountWarmup: async () => legacy, runAccountWarmup: async () => assert.fail() })
  await settle()
  assert.equal(f.controller.ready, false)
  assert.match(text(f.root), /请升级预热插件/)
  f.controller.open([account.id])
  assert.equal(f.controller.dialogOpen, false)
  assert.match(requestHelpers.warmupRequestError({ model: 'gpt\t6', prompt: 'hi', reasoningEffort: 'auto' }), /模型 ID/)
  f.app.unmount()
})
test('a newer batch replaces a stale manual summary instead of leaving it queued forever', async () => {
  let current = snapshot()
  const f = mountToolbar({ readAccountWarmup: async () => current, runAccountWarmup: async () => ({ run }) })
  await settle()
  f.controller.open([account.id])
  await f.controller.submit()
  assert.match(f.controller.status, /已加入预热队列/)
  current = { ...snapshot(), runtime: { ready: true, nextRunAt: null, lastRun: { ...run, id: 'new-automatic', source: 'daily', status: 'completed' } } }
  await f.controller.refresh()
  assert.equal(f.controller.result, undefined)
  assert.match(f.controller.status, /最近预热已结束/)
  f.app.unmount()
})
test('single account manual request freezes the chosen prompt, model and effort without editing automation', async () => {
  const calls = []
  const current = snapshot()
  current.settings.reasoningEffort = 'low'
  const f = mountToolbar({
    readAccountWarmup: async () => current,
    runAccountWarmup: async (value) => { calls.push(value); return { run } },
  })
  await settle()
  f.controller.openAccount({ ...account, name: '单账号', lifecycle: { archived: false } })
  await settle()
  all(f.root, n => n.props['aria-label'] === '本次预热提示词')[0].props.onInput({ target: { value: 'Hello\n请简短回复' } })
  all(f.root, n => n.props['aria-label'] === '本次预热推理强度')[0].props.onInput({ target: { value: 'high' } })
  await settle()
  await f.controller.submit()
  assert.deepEqual(calls[0].accountIds, [account.id])
  assert.equal(calls[0].prompt, 'Hello\n请简短回复')
  assert.equal(calls[0].reasoningEffort, 'high')
  assert.equal(calls[0].model, current.settings.defaultModel)
  assert.equal(current.settings.prompt, 'hi')
  assert.equal(current.settings.reasoningEffort, 'low')
  f.app.unmount()
})
test('account controls block active, archived, quota-protected and unsupported accounts', async () => {
  const current = snapshot()
  const row = { ...account, lifecycle: { archived: false } }
  const f = mountToolbar({ readAccountWarmup: async () => current })
  await settle()
  assert.equal(f.controller.canRun(row), true)
  assert.equal(f.controller.canRun({ ...row, provider: 'xai' }), false)
  assert.equal(f.controller.canRun({ ...row, status: 'disabled' }), false)
  assert.equal(f.controller.canRun({ ...row, quotaProtectionReached: true }), false)
  assert.equal(f.controller.canRun({ ...row, lifecycle: { archived: true } }), false)
  for (const status of ['queued', 'running']) {
    current.accountStates[account.id] = { status }
    await f.controller.refresh()
    assert.equal(f.controller.canRun(row), false)
  }
  f.app.unmount()
})
test('a background status refresh preserves the model catalog and rejects unsupported manual effort', async () => {
  const calls = []
  const catalog = [{ id: 'gpt-6.1-sol', label: '模型', reasoningEfforts: ['low'] }]
  const f = mountToolbar({
    readAccountWarmup: async (input = {}) => ({ ...snapshot(), accounts: [{ ...account, models: input.includeModels ? catalog : null }] }),
    runAccountWarmup: async (value) => { calls.push(value); return { run } },
  })
  await settle()
  f.controller.open([account.id])
  await settle()
  await f.controller.refresh()
  assert.deepEqual(JSON.parse(JSON.stringify(f.controller.models)), catalog)
  f.controller.request.reasoningEffort = 'high'
  await f.controller.submit()
  assert.match(f.controller.submitError, /不支持/)
  assert.equal(calls.length, 0)
  assert.equal(f.controller.pendingSubmission, undefined)
  f.app.unmount()
})
test('warmup badges expire the current window and never turn acceptance or missing data into warmed', async () => {
  const component = sfc('../../plugins/accounts/ui/page/components/AccountWarmupBadge.vue', { '@lucide/vue': { Flame: icon } })
  const now = Date.parse('2026-10-30T08:00:00Z')
  const render = (state, ready = true) => renderToString(vue.createSSRApp({ render: () => vue.h(component, { state, ready, now }) }))
  const current = await render({ status: 'warmed', resetAt: '2026-10-30T09:00:00Z' })
  assert.match(current, /已预热/)
  assert.match(current, /当前 5 小时额度窗口已启动/)
  const expired = await render({ status: 'warmed', resetAt: '2026-10-30T08:00:00Z' })
  assert.match(expired, /待预热/)
  assert.doesNotMatch(expired, /已预热/)
  assert.match(await render({ status: 'unconfirmed', resetAt: null }), /计时待确认/)
  assert.match(await render({ status: 'unknown', resetAt: null }), /结果未知/)
  assert.match(await render(undefined), /状态不可用/)
  assert.match(await render({ status: 'pending' }, false), /状态不可用/)
})
test('real expanded account fields keep independent overrides and null inheritance', async () => {
  const state = vue.reactive({ settings: model.defaultWarmupSettings() })
  const component = sfc('../../plugins/account-warmup/ui/page/components/WarmupAccountRow.vue', { ...base, '@lucide/vue': { ChevronDown: icon, RefreshCw: icon }, '@/api/modules/accountWarmup': requestHelpers, '../model': model })
  const r = renderer()
  const app = r.runtime.createApp({ render: () => vue.h(component, { account, name: '测试账号', settings: state.settings, selected: true, disabled: false, models: [], modelsLoading: false, modelsError: '', onOverride: (value) => { state.settings.accountOverrides = [value] } }) })
  app.mount(r.root)
  all(r.root, n => n.type === 'button' && n.props['aria-expanded'] === false)[0].props.onClick()
  await settle()
  const toggle = label => all(r.root, n => n.props['aria-label'] === label)[0].props.onChange({ target: { checked: true } })
  toggle('测试账号 使用独立时间')
  await settle()
  all(r.root, n => n.props['aria-label'] === '测试账号 独立时间')[0].props.onInput({ target: { value: '10:42' } })
  toggle('测试账号 使用独立模型')
  await settle()
  const value = state.settings.accountOverrides[0]
  assert.equal(value.dailyTime, '10:42')
  assert.equal(value.model, 'gpt-6.1-sol')
  assert.equal(value.resetDelaySeconds, null)
  all(r.root, n => n.props['aria-label'] === '测试账号 使用独立时间')[0].props.onChange({ target: { checked: false } })
  await settle()
  assert.equal(state.settings.accountOverrides[0].dailyTime, null)
  assert.equal(state.settings.dailyTime, '08:00')
  app.unmount()
})
test('polling preserves dirty settings and revision conflicts retain the draft', async () => {
  let current = snapshot()
  const saved = []
  const modules = {
    '@sdk/polling': { usePluginPolling() {} },
    '@/api/modules/accounts': { getAccounts: async () => ({ items: [], page: { totalPages: 1 } }) },
    '@/api/modules/accountWarmup': {
      ...requestHelpers,
      readAccountWarmup: async () => current,
      accountWarmupHistory: async () => ({ runs: [], items: [] }),
      cancelAccountWarmup: async () => ({ run }),
      configureAccountWarmup: async (revision, settings) => { saved.push({ revision, settings }); throw new Error('revision conflict') },
    },
    '@/components/base/BaseToast': { toast: { success() {} } },
    './model': model,
  }
  const { useWarmupSettings } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupSettings.ts'), modules, 'useWarmupSettings')
  const r = renderer()
  let state
  const app = r.runtime.createApp({ setup() { state = useWarmupSettings(); return () => vue.h('div') } })
  app.mount(r.root)
  await settle()
  state.draft.value.dailyTime = '11:15'
  current = snapshot(2)
  current.settings.dailyTime = '09:00'
  await state.refresh(false, true)
  assert.equal(state.draft.value.dailyTime, '11:15')
  assert.equal(state.revision.value, 1)
  assert.equal(state.changedElsewhere.value, true)
  await state.save()
  assert.equal(saved[0].revision, 1)
  assert.equal(state.draft.value.dailyTime, '11:15')
  assert.match(state.saveError.value, /conflict/)
  await state.refresh(true)
  assert.equal(state.draft.value.dailyTime, '09:00')
  assert.equal(state.dirty.value, false)
  app.unmount()
})
test('account names load beyond the first page and history does not claim a timer from acceptance', async () => {
  const pages = []
  const { loadAccountNames } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupSettings.ts'), {
    '@sdk/polling': {},
    '@/api/modules/accounts': { getAccounts: async (input) => { pages.push(input.page); return { items: [{ id: `a${input.page}`, name: `账号${input.page}` }], page: { totalPages: 3 } } } },
    '@/api/modules/accountWarmup': {},
    '@/components/base/BaseToast': {},
    './model': model,
  }, 'loadAccountNames')
  assert.equal((await loadAccountNames()).get('a3').name, '账号3')
  assert.deepEqual(pages, [1, 2, 3])
  const component = sfc('../../plugins/account-warmup/ui/page/components/WarmupHistory.vue', { ...base, '@/components/base/BaseCard.vue': wrapper, '@/components/base/BaseEmpty.vue': wrapper, '../model': model })
  const html = await renderToString(vue.createSSRApp({ render: () => vue.h(component, { runs: [run], items: [{ id: 'item', accountId: account.id, model: 'gpt-6.1-sol', status: 'accepted', httpStatus: 200, durationMs: 20, resetAt: null, timerConfirmed: false }], selectedId: run.id, names: new Map(), error: '', cancelling: false }) }))
  assert.match(html, /计时尚未确认/)
  assert.doesNotMatch(html, /计时已确认/)
})
test('a poll started during save cannot replace the newly saved settings', async () => {
  let resolveSave
  let resolvePoll
  let reads = 0
  const { useWarmupSettings } = evaluate(read('../../plugins/account-warmup/ui/page/useWarmupSettings.ts'), {
    '@sdk/polling': { usePluginPolling() {} },
    '@/api/modules/accounts': { getAccounts: async () => ({ items: [], page: { totalPages: 1 } }) },
    '@/api/modules/accountWarmup': {
      ...requestHelpers,
      readAccountWarmup: async () => ++reads === 1 ? snapshot() : new Promise((resolve) => { resolvePoll = resolve }),
      accountWarmupHistory: async () => ({ runs: [], items: [] }),
      cancelAccountWarmup: async () => ({ run }),
      configureAccountWarmup: () => new Promise((resolve) => { resolveSave = resolve }),
    },
    '@/components/base/BaseToast': { toast: { success() {} } },
    './model': model,
  }, 'useWarmupSettings')
  const r = renderer()
  let state
  const app = r.runtime.createApp({ setup() { state = useWarmupSettings(); return () => vue.h('div') } })
  app.mount(r.root)
  await settle()
  state.draft.value.dailyTime = '12:10'
  const saving = state.save()
  const polling = state.refresh(false, true)
  const saved = snapshot(2)
  saved.settings.dailyTime = '12:10'
  resolveSave(saved)
  await saving
  resolvePoll(snapshot())
  await polling
  assert.equal(state.draft.value.dailyTime, '12:10')
  assert.equal(state.revision.value, 2)
  app.unmount()
})
test('settings validation keeps automation off by default and rejects inverted jitter or bad time zones', () => {
  const value = model.defaultWarmupSettings()
  assert.equal(value.enabled, false)
  assert.equal(model.settingsError(value), '')
  value.jitterMinSeconds = 100
  assert.match(model.settingsError(value), /随机延迟/)
  value.jitterMinSeconds = 5
  value.timezone = 'Mars/Olympus'
  assert.match(model.settingsError(value), /IANA/)
})

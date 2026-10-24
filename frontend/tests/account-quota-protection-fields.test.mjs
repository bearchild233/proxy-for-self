/* eslint-disable no-new-func -- 编译真实 Vue 控件并通过内存 renderer 执行交互 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import * as protection from '../../plugins/accounts/ui/page/utils/quotaProtection.ts'

function source(path) {
  return readFileSync(new URL(path, import.meta.url), 'utf8')
}
function compileComponent(path, modules) {
  const { descriptor } = parse(source(path))
  const code = stripTypeScriptTypes(compileScript(descriptor, { id: 'oauth-action-test', inlineTemplate: true }).content, { mode: 'strip' })
    .replace(/import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]/g, (_, bindings, name) => `const {${bindings.replace(/\bas\b/g, ':')}} = modules[${JSON.stringify(name)}]`)
    .replace(/import\s+(\w+)\s+from\s+['"]([^'"]+)['"]/g, (_, binding, name) => `const ${binding} = modules[${JSON.stringify(name)}]`)
    .replace('export default', 'return')
  return new Function('modules', code)({ vue, ...modules })
}
function renderer() {
  const node = (type, text = '') => ({
    type,
    text,
    props: {},
    children: [],
    parent: null,
    listeners: {},
    addEventListener(event, callback) { this.listeners[event] = callback },
  })
  const runtime = vue.createRenderer({
    createElement: type => node(type),
    createText: text => node('text', text),
    createComment: () => node('comment'),
    insert(child, parent, anchor) {
      if (child.parent)
        child.parent.children.splice(child.parent.children.indexOf(child), 1)
      child.parent = parent
      const index = anchor ? parent.children.indexOf(anchor) : -1
      parent.children.splice(index < 0 ? parent.children.length : index, 0, child)
    },
    remove(child) { child.parent?.children.splice(child.parent.children.indexOf(child), 1) },
    patchProp: (node, key, _, value) => { node.props[key] = value },
    setText: (node, text) => { node.text = text },
    setElementText: (node, text) => {
      node.text = text
      node.children = []
    },
    parentNode: node => node.parent,
    nextSibling: node => node.parent?.children[node.parent.children.indexOf(node) + 1] ?? null,
  })
  return { runtime, root: node('root') }
}
function text(node) {
  return node.text + node.children.map(text).join('')
}
function all(node, predicate) {
  return [...(predicate(node) ? [node] : []), ...node.children.flatMap(child => all(child, predicate))]
}

function mountFields({ split = true, supported = true, enabled = true } = {}) {
  const state = vue.reactive({ enabled, usedPercentLimit: '90', shortTermUsedPercentLimit: '85', weeklyUsedPercentLimit: '95' })
  const button = vue.defineComponent({ props: ['disabled'], setup(props, { attrs, slots }) {
    return () => vue.h('button', { ...attrs, disabled: props.disabled }, slots.default?.())
  } })
  const input = vue.defineComponent({ props: ['modelValue', 'disabled'], emits: ['update:modelValue'], setup(props, { attrs, emit }) {
    return () => vue.h('input', { ...attrs, disabled: props.disabled, value: props.modelValue, onInput: event => emit('update:modelValue', event.target.value) })
  } })
  // 开关使用真实基础组件，确保只有一个总开关且旧网关无法触发编辑。
  const switchComponent = compileComponent('../src/components/base/BaseSwitch.vue', {})
  const fields = compileComponent('../../plugins/accounts/ui/page/components/AccountQuotaProtectionFields.vue', {
    '@/components/base/BaseButton.vue': button,
    '@/components/base/BaseInput.vue': input,
    '@/components/base/BaseSwitch.vue': switchComponent,
    '../utils/quotaProtection': protection,
  })
  const { runtime, root } = renderer()
  const app = runtime.createApp(vue.defineComponent({ setup() {
    return () => vue.h(fields, {
      split,
      supported,
      disabled: false,
      ...Object.fromEntries(Object.entries(state).flatMap(([key, value]) => [
        [key, value],
        [`onUpdate:${key}`, value => state[key] = value],
      ])),
    })
  } }))
  app.mount(root)
  return { state, root, app }
}

test('real OpenAI fields show one switch and independent 5-hour and 7-day preset groups', async () => {
  const f = mountFields()
  try {
    assert.equal(all(f.root, node => node.props.role === 'switch').length, 1)
    assert.equal(all(f.root, node => node.type === 'input' && node.props.type === 'number').length, 2)
    assert.equal(all(f.root, node => node.type === 'button').length, 6)
    for (const label of ['5 小时养号阈值 90%', '7 天养号阈值 85%']) {
      const preset = all(f.root, node => node.props['aria-label'] === label)[0]
      assert.ok(preset)
      preset.props.onClick()
      await vue.nextTick()
    }
    assert.equal(f.state.shortTermUsedPercentLimit, '90')
    assert.equal(f.state.weeklyUsedPercentLimit, '85')
    assert.equal(f.state.usedPercentLimit, '90')
    const shortInput = all(f.root, node => node.props['aria-label'] === '5 小时养号已使用百分比阈值')[0]
    shortInput.props.onInput({ target: { value: '90.5' } })
    await vue.nextTick()
    assert.equal(shortInput.props['aria-invalid'], true)
    assert.equal(f.state.weeklyUsedPercentLimit, '85')
    assert.match(text(f.root), /5 小时请输入 1 至 99/)
    const toggle = all(f.root, node => node.props.role === 'switch')[0]
    toggle.checked = false
    toggle.listeners.change({ target: toggle })
    await vue.nextTick()
    assert.equal(f.state.enabled, false)
    assert.equal(all(f.root, node => node.type === 'input' && node.props.type === 'number').length, 0)
    assert.equal(all(f.root, node => node.props.role === 'alert').length, 0)
  }
  finally { f.app.unmount() }
})

test('real legacy OpenAI fields disable the switch and explain the required gateway upgrade', () => {
  const f = mountFields({ supported: false })
  try {
    const switches = all(f.root, node => node.props.role === 'switch')
    assert.equal(switches.length, 1)
    assert.equal(switches[0].props.disabled, true)
    assert.equal(all(f.root, node => node.type === 'button').length, 0)
    assert.match(text(f.root), /不支持分周期养号设置，请先更新网关/)
  }
  finally { f.app.unmount() }
})

test('real xAI fields show a billing threshold and a single preset group', async () => {
  const f = mountFields({ split: false })
  try {
    assert.equal(all(f.root, node => node.props.role === 'switch').length, 1)
    assert.equal(all(f.root, node => node.type === 'input' && node.props.type === 'number').length, 1)
    assert.equal(all(f.root, node => node.type === 'button').length, 3)
    assert.match(text(f.root), /账期使用达到/)
    const preset = all(f.root, node => node.props['aria-label'] === '账期养号阈值 95%')[0]
    preset.props.onClick()
    await vue.nextTick()
    assert.equal(f.state.usedPercentLimit, '95')
    assert.equal(f.state.shortTermUsedPercentLimit, '85')
    assert.equal(f.state.weeklyUsedPercentLimit, '95')
  }
  finally { f.app.unmount() }
})

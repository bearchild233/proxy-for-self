/* eslint-disable no-new-func -- 编译真实账号身份组件，验证 Vue 条件分支渲染 */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
// eslint-disable-next-line test/no-import-node-test -- 与仓库共用内置 runner
import test from 'node:test'
import * as vue from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import { renderToString } from 'vue/server-renderer'

const source = readFileSync(new URL('../../plugins/accounts/ui/page/components/AccountIdentityCell.vue', import.meta.url), 'utf8')
const { descriptor } = parse(source)
const code = stripTypeScriptTypes(compileScript(descriptor, { id: 'account-identity-notes', inlineTemplate: true }).content, { mode: 'strip' })
  .replace(/import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]/g, (_, bindings, name) => `const {${bindings.replace(/\bas\b/g, ':')}} = modules[${JSON.stringify(name)}]`)
  .replace(/import\s+(\w+)\s+from\s+['"]([^'"]+)['"]/g, (_, binding, name) => `const ${binding} = modules[${JSON.stringify(name)}]`)
  .replace('export default', 'return')
const icon = () => vue.h('i')
const AccountIdentityCell = new Function('modules', code)({
  vue,
  '@boxicons/vue': { Grok: icon, Openai: icon },
  '@lucide/vue': { CircleHelp: icon },
  './AccountNotesPopover.vue': vue.defineComponent({
    props: ['notes'],
    setup: props => () => vue.h('span', { 'data-notes': '' }, props.notes),
  }),
  '@kit/accounts/AccountPlanBadge.vue': vue.defineComponent({
    props: ['planTypeDisplay'],
    setup: props => () => vue.h('span', { 'data-plan': '' }, props.planTypeDisplay),
  }),
})
const account = { id: 'fixture', name: '备用账号', email: 'fixture@example.invalid', provider: 'openai', planType: 'pro', planTypeDisplay: 'Pro', notes: '下月续费' }
function render(props = {}, slots) {
  return renderToString(vue.createSSRApp({ render: () => vue.h(AccountIdentityCell, { account, ...props }, slots) }))
}

test('account list renders notes alongside the secondary plan badge', async () => {
  const html = await render({ showNotes: true, showPlan: true, metaPosition: 'secondary' })
  assert.match(html, /data-plan[^>]*>Pro</)
  assert.match(html, /data-notes[^>]*>下月续费</)
})

test('secondary custom metadata does not hide notes', async () => {
  const html = await render({ showNotes: true, metaPosition: 'secondary' }, { meta: () => vue.h('span', '自定义状态') })
  assert.match(html, /自定义状态/)
  assert.match(html, /data-notes[^>]*>下月续费</)
})

test('notes visibility and existing email fallback remain compatible', async () => {
  assert.match(await render({ showNotes: true }), /data-notes[^>]*>下月续费</)
  assert.doesNotMatch(await render(), /data-notes/)
  const withoutNotes = { ...account, notes: null }
  assert.match(await render({ account: withoutNotes, showNotes: true }), />fixture@example\.invalid</)
  const secondaryPlan = await render({ account: withoutNotes, showNotes: true, showPlan: true, metaPosition: 'secondary' })
  assert.match(secondaryPlan, /data-plan[^>]*>Pro</)
  assert.doesNotMatch(secondaryPlan, /data-notes|>fixture@example\.invalid</)
})

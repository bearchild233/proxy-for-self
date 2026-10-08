/* eslint-disable no-new-func -- 测试执行实际源码并注入受控依赖 */
import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { createSSRApp, defineComponent, h } from 'vue'
import { compileScript, parse } from 'vue/compiler-sfc'
import { renderToString } from 'vue/server-renderer'

const filename = fileURLToPath(new URL('../src/plugins/PluginFrame.vue', import.meta.url))
const descriptor = parse(readFileSync(filename, 'utf8'), { filename }).descriptor
const compiled = compileScript(descriptor, { id: 'scroll-contract', fs: { fileExists: existsSync, readFile: path => readFileSync(path, 'utf8') } })
const declaration = compiled.content.match(/\n\s*props: (\{[\s\S]*?\}),\n\s*emits:/)
assert.ok(declaration, 'compiled host props must be available')
const props = new Function(`return (${declaration[1]})`)()
const Host = defineComponent({ props, setup: p => () => h('div', p.scrollable === false ? 'popup-managed-scroll' : 'page-scrollbar') })
const render = values => renderToString(createSSRApp(Host, { plugin: {}, page: {}, ...values }))
test('normal plugin pages keep scrollbar when Boolean prop is omitted', async () => {
  assert.equal(await render({}), '<div>page-scrollbar</div>')
  assert.equal(await render({ scrollable: true }), '<div>page-scrollbar</div>')
})
test('only explicit popup opt-out removes outer scrollbar', async () => {
  assert.equal(await render({ scrollable: false }), '<div>popup-managed-scroll</div>')
})

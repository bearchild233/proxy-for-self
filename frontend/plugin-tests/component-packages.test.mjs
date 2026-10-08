import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { readdir, readFile } from 'node:fs/promises'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import * as pinia from 'pinia'
import * as vue from 'vue'
import * as router from 'vue-router'
import * as columns from '../src/components/base/BaseTable/columns.ts'

test('actual component packages share Vue and prepare without business calls', async () => {
  const root = new URL('../../.build/plugins/', import.meta.url)
  let checked = 0
  const ids = process.env.PLUGIN_ID
    ? [process.env.PLUGIN_ID]
    : await readdir(root)
  for (const id of ids) {
    let manifest
    try {
      manifest = JSON.parse(
        await readFile(new URL(`${id}/package/plugin.json`, root), 'utf8'),
      )
    }
    catch {
      continue
    }
    if (manifest.runtime !== 'vue-component')
      continue
    const source = await readFile(
      new URL(`${id}/package/ui/module.js`, root),
      'utf8',
    )
    assert.ok(
      !source.includes('__vueParentComponent'),
      `${id} must not bundle Vue runtime`,
    )
    assert.ok(
      !source.includes('createApp('),
      `${id} must not start a second app`,
    )
    const module = await import(
      `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`,
    )
    assert.equal(typeof module.create, 'function')
    let calls = 0
    const bridge = {
      context: {
        active: false,
        role: 'admin',
        route: '/',
        plugins: [],
        apiBaseUrl: 'http://test/v1',
      },
      events: new EventTarget(),
      call: () => {
        calls++
        throw new Error('unexpected business request')
      },
      host: () => {
        calls++
        throw new Error('unexpected host request')
      },
    }
    let pages
    try {
      pages = module.create(
        {
          vue,
          router,
          pinia,
          shared: new Proxy(
            {},
            {
              get: (_, key) =>
                key === 'components/base/BaseTable/columns' ? columns : {},
            },
          ),
        },
        bridge,
      )
    }
    catch (error) {
      throw new Error(`${id}: ${error.message}`)
    }
    for (const page of manifest.pages) {
      assert.equal(
        typeof pages[page.id].component,
        'object',
        `${id}/${page.id}`,
      )
    }
    assert.equal(calls, 0, `${id} must not fetch business data before opening`)
    checked++
  }
  assert.equal(checked, process.env.PLUGIN_ID ? 1 : 12)
})

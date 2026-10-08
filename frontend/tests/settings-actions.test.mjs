import assert from 'node:assert/strict'
// eslint-disable-next-line test/no-import-node-test -- 项目统一使用 Node 内置测试 runner
import test from 'node:test'
import { settingsActions } from '../src/plugins/settingsActions.ts'

test('settings header defaults actions to disabled and accepts only boolean state', () => {
  assert.deepEqual(settingsActions({}), { changed: false, saving: false, saveDisabled: true, resetDisabled: true })
  assert.deepEqual(settingsActions({ changed: true, saving: false, saveDisabled: false, resetDisabled: false }), { changed: true, saving: false, saveDisabled: false, resetDisabled: false })
  assert.equal(settingsActions({ saveDisabled: 'false' }).saveDisabled, true)
})

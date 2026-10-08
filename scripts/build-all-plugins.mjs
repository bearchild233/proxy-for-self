import { readFile } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
const policies = JSON.parse(await readFile(new URL('../packages/plugin-sdk/policies.json', import.meta.url), 'utf8'))
for (const id of Object.keys(policies)) {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('./build-plugin.mjs', import.meta.url)), id], { stdio: 'inherit' })
  if (result.status !== 0) process.exit(result.status || 1)
}

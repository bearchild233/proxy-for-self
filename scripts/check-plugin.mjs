import { mkdir, readFile, writeFile, unlink } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import path from 'node:path'
import { spawnSync } from 'node:child_process'

const root = fileURLToPath(new URL('../', import.meta.url))
const id = process.argv[2]
if (!/^[a-z][a-z0-9-]{0,63}$/.test(id || '')) throw new Error('Invalid plugin ID')
const manifest = JSON.parse(await readFile(path.join(root, 'plugins', id, 'plugin.json'), 'utf8'))
if (manifest.id !== id) throw new Error('Plugin ID mismatch')
await mkdir(path.join(root, '.build'), { recursive: true })
const config = path.join(root, '.build', `check-${id}.json`)
await writeFile(config, JSON.stringify({
  extends: path.join(root, 'frontend/tsconfig.app.json'),
  compilerOptions: { composite: false, incremental: true, tsBuildInfoFile: path.join(root, 'frontend/node_modules/.tmp', `${id}.tsbuildinfo`) },
  include: [path.join(root, 'plugins', id, 'ui/**/*.ts'), path.join(root, 'plugins', id, 'ui/**/*.vue'), path.join(root, 'frontend/env.d.ts')],
}))
try {
  const result = spawnSync(process.execPath, [path.join(root, 'frontend/node_modules/vue-tsc/bin/vue-tsc.js'), '--project', config, '--pretty', 'false'], { stdio: 'inherit' })
  process.exitCode = result.status || (result.error ? 1 : 0)
}
finally { await unlink(config) }

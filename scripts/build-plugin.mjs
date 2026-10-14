import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { createRequire } from 'node:module'
import { build } from '../frontend/node_modules/vite/dist/node/index.js'

const root = fileURLToPath(new URL('../', import.meta.url))
const id = process.argv[2]
if (!/^[a-z][a-z0-9-]{0,63}$/.test(id || ''))
  throw new Error('Usage: node scripts/build-plugin.mjs <registered-plugin-id>')
const pluginRoot = path.join(root, 'plugins', id)
const manifest = JSON.parse(await readFile(path.join(pluginRoot, 'plugin.json'), 'utf8'))
if (manifest.id !== id || manifest.protocol !== 2 || manifest.kind !== 'ui')
  throw new Error('Unsupported plugin manifest')
const output = path.join(root, '.build', 'plugins', id)
const packageDir = path.resolve(output, 'package')
if (!packageDir.startsWith(path.resolve(root, '.build/plugins') + path.sep)) throw new Error('Invalid package path')

// 服务插件的 Rust crate 约定位于 plugins/<插件ID>/service/Cargo.toml，并由 plugins/Cargo.toml workspace 管理。
const serviceCrate = path.join(pluginRoot, 'service', 'Cargo.toml')
if (existsSync(serviceCrate) !== Boolean(manifest.service))
  throw new Error('服务 crate 与 plugin.json 的 service 段必须同时存在或同时缺失')
const wslDistribution = process.env.CPR_WSL_DISTRO || 'ApiHubBuild-20260928'

// Windows 路径转为 WSL 挂载路径（例如 D:\\a\\b -> /mnt/d/a/b）。
function toWsl(value) {
  const match = /^([A-Za-z]):[\\/](.*)$/.exec(value)
  return match ? `/mnt/${match[1].toLowerCase()}/${match[2].replaceAll('\\', '/')}` : value.replaceAll('\\', '/')
}

// 按本机 WSL 入口构建服务二进制（release），再把 WSL target 中的文件取到包目录并写入哈希。
// 只校验文件头格式；内核与 updater 会各自重新校验，不信任这里的结果。
async function addServiceBinary(serviceManifest, destination) {
  const binary = serviceManifest.service.binary
  if (!/^service\/[a-z][a-z0-9_-]*$/.test(binary)) throw new Error('Invalid service binary path')
  const name = binary.slice('service/'.length)
  const crate = await readFile(serviceCrate, 'utf8')
  const declared = /^name\s*=\s*"([^"]+)"/m.exec(crate)?.[1]
  if (declared !== name) throw new Error('服务 crate 的包名必须与 service.binary 的文件名一致')
  const linuxRoot = toWsl(root)
  const script = `${linuxRoot}/scripts/build-plugins-local.sh`
  // 安装包沿用 plugins/Cargo.toml 的正式 release 配置，避免快速本地构建产物超出既有单文件上限。
  execFileSync('wsl.exe', ['-d', wslDistribution, '-u', 'root', '--', 'env', 'CARGO_PROFILE_RELEASE_LTO=thin', 'CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1', 'bash', script, linuxRoot, '4096', '3', 'release', name], { stdio: 'inherit' })
  const built = `/work/proxy-for-self/plugins-target/release/${name}`
  const target = path.join(destination, binary)
  await mkdir(path.dirname(target), { recursive: true })
  execFileSync('wsl.exe', ['-d', wslDistribution, '-u', 'root', '--', 'cp', built, toWsl(target)], { stdio: 'inherit' })
  const data = await readFile(target)
  const elf = data.length >= 20 && data.readUInt32BE(0) === 0x7f454c46 && data[4] === 2 && data[5] === 1
    && [2, 3].includes(data.readUInt16LE(16)) && data.readUInt16LE(18) === 62
  if (!elf) throw new Error('构建产物不是 x86-64 ELF 可执行文件')
  return createHash('sha256').update(data).digest('hex')
}

if (!existsSync(path.join(pluginRoot, 'ui/entries.json'))) {
  // 纯服务插件：没有界面代码，包只含清单、许可证与服务二进制。
  await rm(packageDir, { recursive: true, force: true })
  await mkdir(packageDir, { recursive: true })
  manifest.resources = {}
  const license = await readFile(path.join(root, 'LICENSE'), 'utf8')
  await writeFile(path.join(packageDir, 'LICENSE'), license)
  manifest.resources.LICENSE = createHash('sha256').update(license).digest('hex')
  manifest.resources[manifest.service.binary] = await addServiceBinary(manifest, packageDir)
  await writeFile(path.join(packageDir, 'plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`)
  process.stdout.write(`${JSON.stringify({ id, version: manifest.version, directory: packageDir, bytes: 0, runtime: 'service', gatewayBuild: false })}\n`)
} else {
const component = manifest.runtime === 'vue-component'
const entries = JSON.parse(await readFile(path.join(pluginRoot, 'ui/entries.json'), 'utf8'))
const frontend = path.join(root, 'frontend')
const require = createRequire(path.join(frontend, 'package.json'))
const sdk = path.join(root, 'packages/plugin-sdk')
const normalize = value => value.replaceAll('\\', '/')
const replacements = new Map([
  [normalize(path.join(frontend, 'src/composables/useCopyText.ts')), path.join(sdk, 'copy.ts')],
  [normalize(path.join(frontend, 'src/api/request.ts')), path.join(sdk, 'request.ts')],
  [normalize(path.join(frontend, 'src/composables/useDownload.ts')), path.join(sdk, 'download.ts')],
  [normalize(path.join(frontend, 'src/plugins/catalog.ts')), path.join(sdk, 'catalog.ts')],
  [normalize(path.join(frontend, 'src/plugins/PluginSlot.vue')), path.join(sdk, 'PluginSlot.vue')],
  [normalize(path.join(frontend, 'src/utils/serviceUrl.ts')), path.join(sdk, 'service-url.ts')],
  [normalize(path.join(frontend, 'src/api/modules/client-models.ts')), path.join(sdk, 'client-models.ts')],
])
const imports = Object.values(entries).map((entry, index) => `import Page${index} from ${JSON.stringify(normalize(path.join(pluginRoot, 'ui', entry.source)))}`).join('\n')
const pages = Object.entries(entries).map(([key, entry], index) => `${JSON.stringify(key)}:{component:Page${index},route:${JSON.stringify(entry.route || '/')},name:${JSON.stringify(entry.route === '/' ? 'dashboard' : (entry.route || '/').slice(1).replaceAll('/', '-') || 'main')}}`).join(',')
const shared = new Map()
const external = ['vue', 'vue-router', 'pinia', 'proxy:bridge']
const globals = { vue: 'runtime.vue', 'vue-router': 'runtime.router', pinia: 'runtime.pinia', 'proxy:bridge': 'bridge' }
const virtualEntry = '\0plugin-entry'
const aliases = Object.fromEntries(Object.keys(JSON.parse(await readFile(path.join(frontend, 'package.json'), 'utf8')).dependencies).map(name => [name, path.join(frontend, 'node_modules', name)]))
await build({
  root: frontend,
  configFile: path.join(frontend, 'vite.config.ts'),
  define: { 'process.env.NODE_ENV': JSON.stringify('production') },
  plugins: [{
    name: 'isolated-plugin-transport', enforce: 'pre',
    resolveId(source, importer) {
      if (component) {
        const library = ['vue', 'vue-router', 'pinia'].find(name => source === name || normalize(source) === normalize(aliases[name]) || normalize(source).startsWith(normalize(aliases[name]) + '/'))
        if (library) return { id: library, external: true }
      }
      if (source === 'plugin-entry' || normalize(source) === normalize(path.join(frontend, 'plugin-entry'))) return virtualEntry
      if (component && external.includes(source)) return { id: source, external: true }
      const resolved = source.startsWith('@sdk/') ? path.join(sdk, source.slice(5)) : source.startsWith('@/') ? path.join(frontend, 'src', source.slice(2))
        : importer && source.startsWith('.') ? path.resolve(path.dirname(importer), source) : source
      const clean = normalize(resolved).replace(/\.(ts|vue)$/, '')
      if (component && clean === normalize(path.join(sdk, 'ui'))) return path.join(sdk, 'component-ui.ts')
      const relative = normalize(path.relative(path.join(frontend, 'src'), resolved))
      if (component && (relative.startsWith('components/base/') || ['stores/modules/auth', 'stores/modules/theme'].includes(relative.replace(/\.ts$/, '')))) {
        const key = relative.replace(/\.ts$/, '').replace(/\.vue$/, '')
        const name = 'proxy:shared/' + key
        shared.set(name, key); globals[name] = `runtime.shared[${JSON.stringify(key)}]`
        return { id: name, external: true }
      }
      return replacements.get(normalize(resolved)) || replacements.get(`${normalize(resolved)}.ts`)
    },
    load(source) {
      if (source === virtualEntry && component) return `${imports}\nimport ${JSON.stringify(normalize(path.join(frontend, 'src/styles/index.css')))};export const pages={${pages}}`
      if (source === virtualEntry)
        return `${imports}\nimport {mount} from ${JSON.stringify(normalize(path.join(sdk, 'bootstrap.ts')))};void mount({${pages}})`
    },
  }],
  resolve: { alias: { '@vueuse/integrations/useSortable': require.resolve('@vueuse/integrations/useSortable'), ...aliases, '@plugins': path.join(root, 'plugins'), '@sdk': sdk, '@kit': path.join(root, 'packages/ui-kit') } },
  build: {
    outDir: path.join(output, 'compiled'), emptyOutDir: true, cssCodeSplit: false,
    lib: { entry: 'plugin-entry', name: 'ProxyPlugin', formats: ['iife'], fileName: () => 'main.js' },
    rolldownOptions: { output: { codeSplitting: false, globals: name => name.startsWith('proxy:shared/') ? `runtime.shared[${JSON.stringify(name.slice(13))}]` : globals[name] } },
  },
})
const compiled = path.join(output, 'compiled')
const files = await readdir(compiled)
const css = (await Promise.all(files.filter(file => file.endsWith('.css')).map(file => readFile(path.join(compiled, file), 'utf8')))).join('\n')
const js = await readFile(path.join(compiled, 'main.js'), 'utf8')
const html = `<!doctype html><html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>${css.replaceAll('</style', '<\\/style')}</style></head><body><div id="app"></div><script>${js.replaceAll('</script', '<\\/script')}</script></body></html>`
await rm(packageDir, { recursive: true, force: true })
await mkdir(path.join(packageDir, 'ui'), { recursive: true })
// Component packages export a factory; preloading does not mount pages or run their API calls.
// Vue and shared UI are provided by the host, never a second createApp.
const entry = component ? 'ui/module.js' : 'ui/index.html'
const postcss = require('postcss')
const rules = postcss.parse(css)
if (component) {
  for (const node of [...rules.nodes]) {
    if (node.type === 'atrule' && ['layer', 'keyframes', 'property', 'supports'].includes(node.name)) {
      if (node.name === 'layer' && node.nodes && node.params !== 'utilities') node.remove()
    } else if (!(node.type === 'rule' && node.selector.includes('[data-v-'))) node.remove()
  }
}
const content = component ? `export const hostApi=1;export const requirements=${JSON.stringify([...shared.values()].sort())};export const css=${JSON.stringify(rules.toString())};export function create(runtime,bridge){${js}\nreturn ProxyPlugin.pages;}\n` : html
await writeFile(path.join(packageDir, entry), content)
manifest.pages = manifest.pages.map(page => ({ ...page, entry }))
manifest.resources = { [entry]: createHash('sha256').update(content).digest('hex') }
const license = await readFile(path.join(root, 'LICENSE'), 'utf8')
await writeFile(path.join(output, 'package/LICENSE'), license)
manifest.resources.LICENSE = createHash('sha256').update(license).digest('hex')
for (const asset of manifest.dataFiles || []) {
  if (!/^[a-z][a-z0-9.-]*$/.test(asset)) throw new Error('Invalid data asset')
  const data = await readFile(path.join(pluginRoot, asset))
  await writeFile(path.join(output, 'package', asset), data)
  manifest.resources[asset] = createHash('sha256').update(data).digest('hex')
}
for (const entry of [manifest.worker, manifest.policyWorker].filter(Boolean)) {
  if (!/^worker\/[a-z][a-z0-9_-]*\.py$/.test(entry)) throw new Error('Invalid worker entry')
  const worker = await readFile(path.join(pluginRoot, entry))
  await mkdir(path.dirname(path.join(output, 'package', entry)), { recursive: true })
  await writeFile(path.join(output, 'package', entry), worker)
  manifest.resources[entry] = createHash('sha256').update(worker).digest('hex')
}
if (manifest.service) manifest.resources[manifest.service.binary] = await addServiceBinary(manifest, packageDir)
await writeFile(path.join(output, 'package/plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`)
process.stdout.write(`${JSON.stringify({ id, version: manifest.version, directory: path.join(output, 'package'), bytes: Buffer.byteLength(content), runtime: manifest.runtime || 'iframe', gatewayBuild: false })}\n`)
}

import { createHash } from 'node:crypto'
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
const component = manifest.runtime === 'vue-component'
const entries = JSON.parse(await readFile(path.join(pluginRoot, 'ui/entries.json'), 'utf8'))
const output = path.join(root, '.build', 'plugins', id)
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
const packageDir = path.resolve(output, 'package')
if (!packageDir.startsWith(path.resolve(root, '.build/plugins') + path.sep)) throw new Error('Invalid package path')
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
await writeFile(path.join(output, 'package/plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`)
process.stdout.write(`${JSON.stringify({ id, version: manifest.version, directory: path.join(output, 'package'), bytes: Buffer.byteLength(content), runtime: manifest.runtime || 'iframe', gatewayBuild: false })}\n`)

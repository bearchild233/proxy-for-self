import * as pinia from 'pinia'
import * as vue from 'vue'
import * as router from 'vue-router'
import * as auth from '@/stores/modules/auth'
import * as theme from '@/stores/modules/theme'

// 宿主接口 v1。公共组件与运行库只在主应用实例化，不随插件重复打包。
const files = import.meta.glob('../components/base/**/*.{vue,ts}', { eager: true })
const shared: Record<string, unknown> = Object.fromEntries(Object.entries(files).map(([path, module]) => [path.replace('../', '').replace(/\.(vue|ts)$/, ''), path.endsWith('.vue') ? (module as { default: unknown }).default : module]))
for (const [key, module] of Object.entries(shared)) {
  if (key.endsWith('/index'))
    shared[key.slice(0, -6)] = module
}
shared['stores/modules/auth'] = auth
shared['stores/modules/theme'] = theme
export const componentRuntime = { vue, router, pinia, shared }

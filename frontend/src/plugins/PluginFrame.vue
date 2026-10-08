<script setup lang="ts">
import type { Component } from 'vue'
import type { PluginBridge } from '../../../packages/plugin-sdk/ui'
import type { PlatformPage, PlatformPlugin } from './platform'
import type { SettingsActions } from './settingsActions'
import type { ThemeColorId, ThemeCustomization, ThemeMode } from '@/theme'
import { useResizeObserver } from '@vueuse/core'
import { defineComponent, h, markRaw, onErrorCaptured, onMounted, onScopeDispose, provide, reactive, ref, shallowRef, watch } from 'vue'
import { routeLocationKey, useRouter } from 'vue-router'

import request from '@/api/request'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { useAuthStore } from '@/stores/modules/auth'
import { useThemeStore } from '@/stores/modules/theme'
import { pinComponent, prepareComponent } from './componentResources'
import { componentRuntime } from './componentRuntime'
import { loadPluginPage } from './pageResources'
import { openPlugin, platformPlugins, refreshPlatform } from './platform'
import { settingsActions } from './settingsActions'
import './component-layout.css'

const props = withDefaults(defineProps<{
  plugin: PlatformPlugin
  page: PlatformPage
  active?: boolean
  scrollable?: boolean
  context?: Record<string, unknown>
}>(), { scrollable: true })
const emit = defineEmits<{
  settingsActions: [value: SettingsActions]
  ready: []
  failed: []
  close: [
  ]
}>()
const router = useRouter()
const auth = useAuthStore()
const themeStore = useThemeStore()
const frame = ref<HTMLIFrameElement>()
const componentRoot = shallowRef<Component>()
const container = ref<HTMLElement>()
const contentHeight = ref('400px')
let componentBridge: (PluginBridge & { events: EventTarget }) | undefined
let unpin: (() => void) | undefined
useResizeObserver(container, (entries) => {
  const height = entries[0]?.contentRect.height
  if (height)
    contentHeight.value = `${Math.max(0, height - (window.matchMedia('(min-width: 961px)').matches ? 24 : 16))}px`
})
const html = ref('')
const error = ref('')
let token = ''
let generation = 0
let pending = 0
let bound: { id: string, version: string } | undefined
let lease: string | undefined, leaseTimer: ReturnType<typeof setInterval> | undefined
let releasePage: (() => void) | undefined
function releaseLease() {
  unpin?.()
  unpin = undefined
  clearInterval(leaseTimer)
  releasePage?.()
  releasePage = undefined
  lease = undefined
}
function themeConfiguration() {
  return { mode: themeStore.themeMode, color: themeStore.themeColor, customColor: themeStore.customThemeColor, customization: themeStore.themeCustomization }
}
function sendContext() {
  if (componentBridge) {
    Object.assign(componentBridge.context, { active: props.active !== false, plugins: platformPlugins.value })
    componentBridge.events.dispatchEvent(new Event('plugin-context'))
  }
  frame.value?.contentWindow?.postMessage(JSON.parse(JSON.stringify({ token, event: 'context', active: props.active !== false, plugins: platformPlugins.value, themeConfiguration: themeConfiguration(),
  })), '*')
}
let readyTimer: ReturnType<typeof setTimeout> | undefined
async function load() {
  releaseLease()
  const run = ++generation
  error.value = ''
  html.value = ''
  componentRoot.value = undefined
  componentBridge = undefined
  token = crypto.randomUUID()
  clearTimeout(readyTimer)
  if (!props.plugin.contentVersion)
    return
  bound = { id: props.plugin.id, version: props.plugin.contentVersion }
  const current = bound
  try {
    const result = await loadPluginPage(bound.id, bound.version, props.page.id)
    if (run !== generation) {
      result.release()
      return
    }
    lease = result.lease
    releasePage = result.release
    leaseTimer = setInterval(() => {
      if (bound && lease) {
        void request({ url: '/api/plugin-platform/lease', method: 'POST', data: { ...bound, lease }, silent: true,
        }).catch(() => {
          error.value = '插件已停用或版本已失效，请重新打开'
          html.value = ''
          componentRoot.value = undefined
          releaseLease()
        })
      }
    }, 30000)
    const styles = document.documentElement.style
    const theme = Object.fromEntries(Array.from(styles).filter(key => key.startsWith('--cp-')).map(key => [key, styles.getPropertyValue(key)]))
    const context = { ...props.context, active: props.active !== false, settingsShell: props.page.layout === 'settings', presentation: props.page.route ? 'page' : 'popup', route: props.page.route || '/', role: auth.session?.role, apiBaseUrl: `${location.origin}/v1`, theme, themeConfiguration: themeConfiguration(), plugins: platformPlugins.value }
    if (props.page.runtime === 'vue-component') {
      const key = `${current.id}/${current.version}`
      unpin = pinComponent(key)
      const module = await prepareComponent(key, result.html)
      if (run !== generation)
        return
      const eventTarget = new EventTarget()
      const localBridge: PluginBridge & { events: EventTarget } = {
        context: reactive(context) as PluginBridge['context'],
        events: eventTarget,
        async call<T>(name: string, input: unknown): Promise<T> {
          if (run !== generation || !platformPlugins.value.some(p => p.id === current.id))
            throw new Error('插件已停用')
          return request<T>({ url: `/api/plugin-platform/call/${current.id}/${current.version}/${name}`, method: 'POST', data: input, silent: true })
        },
        host: (name, input) => hostOperation(name, input as Record<string, unknown>) as never,
      }
      componentBridge = localBridge
      const pages = module.create(componentRuntime, localBridge)
      const page = pages[props.page.id]
      if (!page?.component)
        throw new Error('组件插件缺少页面')
      componentRoot.value = markRaw(defineComponent({
        setup() {
          // 保持每页路由身份，隐藏的设置页不会跟随全局路由改变内容。
          provide(routeLocationKey, reactive({ ...router.resolve(page.route), name: page.name }))
          onMounted(() => emit('ready'))
          onErrorCaptured(() => {
            error.value = '插件运行失败，可在插件管理中回退'
            emit('failed')
            return false
          })
          return () => h(page.component, { active: props.active !== false })
        },
      }))
      return
    }
    const bridge = `const pending=new Map();function send(type,name,input){return new Promise((resolve,reject)=>{const id=crypto.randomUUID(),timer=setTimeout(()=>{pending.delete(id);reject(new Error('插件操作超时'))},45000);pending.set(id,{resolve,reject,timer});parent.postMessage({type,token:${JSON.stringify(token)},id,name,input},'*')})};addEventListener('message',e=>{if(e.source!==parent||e.data?.token!==${JSON.stringify(token)})return;if(e.data.event==='settings-action'){dispatchEvent(new CustomEvent('plugin-settings-action',{detail:e.data.action}));return};if(e.data.event==='context'){window.proxyPlugin.context.active=e.data.active;window.proxyPlugin.context.plugins=e.data.plugins;window.proxyPlugin.context.themeConfiguration=e.data.themeConfiguration;dispatchEvent(new CustomEvent('plugin-context',{detail:e.data}));return};const p=pending.get(e.data.id);if(!p)return;pending.delete(e.data.id);clearTimeout(p.timer);e.data.error?p.reject(new Error(e.data.error)):p.resolve(e.data.value)});window.proxyPlugin={context:${JSON.stringify(context).replaceAll('<', '\\u003c')},call:(name,input)=>send('call',name,input),host:(name,input)=>send('host',name,input)};addEventListener('error',()=>send('host','failed',{}).catch(()=>{}));`
    const csp = `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'`
    html.value = `<meta http-equiv="Content-Security-Policy" content="${csp}"><script>${bridge}<\/script>${result.html}`
    readyTimer = setTimeout(() => {
      if (run === generation) {
        error.value = '插件未能完成加载，可重试或在插件管理中回退版本。'
        emit('failed')
      }
    }, 12000)
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : '插件加载失败'
    emit('failed')
  }
}
async function hostOperation(name: string, input: Record<string, unknown>) {
  switch (name) {
    case 'settings-actions':
      if (bound?.id !== 'gateway-settings' || props.page.layout !== 'settings')
        throw new Error('设置操作未授权')
      emit('settingsActions', settingsActions(input))
      return true
    case 'theme': {
      if (bound?.id !== 'appearance' && bound?.id !== 'usage-analytics')
        throw new Error('不能修改主题')
      themeStore.setThemeConfiguration(input as unknown as {
        mode: ThemeMode
        color: ThemeColorId
        customColor: string
        customization: ThemeCustomization
      })
      return true
    }
    case 'ready':
      clearTimeout(readyTimer)
      sendContext()
      emit('ready')
      return true
    case 'failed':
      clearTimeout(readyTimer)
      error.value = '插件运行失败，请重新打开或回退版本。'
      emit('failed')
      return true
    case 'catalog':
      await refreshPlatform()
      return platformPlugins.value
    case 'navigate': {
      const path = String(input.path)
      if (!path.startsWith('/') || path.startsWith('//') || (!platformPlugins.value.some(p => p.pages.some(page => page.route === path.split('?')[0])) && path !== '/login'))
        throw new Error('导航不可用')
      if (path === '/login')
        await auth.checkAuth()
      await router.push(path)
      return true
    }
    case 'open': {
      const slots: Record<string, string[]> = {
        'accounts': ['account-menu', 'account-detail'],
        'groups': ['group-menu'],
        'keys': ['key-menu', 'key-config', 'key-editor'],
        'usage-analytics': ['key-config', 'key-usage', 'usage-detail', 'dashboard'],
        'gateway-settings': ['settings'],
        'proxy-management': ['proxy-menu'],
        'client-access': ['settings', 'key-config'],
      }
      const id = String(input.id)
      const target = platformPlugins.value.find(plugin => plugin.id === id)
      const page = target?.pages.find(page => page.slot && (!input.pageId || page.id === input.pageId) && bound && slots[bound.id]?.includes(page.slot))
      if (!page)
        throw new Error('不能打开该位置的插件')
      await openPlugin(id, (input.context || {}) as Record<string, unknown>, page.id)
      return true
    }
    case 'close':
      emit('close')
      return true
    case 'copy':
      await navigator.clipboard.writeText(String(input.text))
      return true
    case 'download': {
      const fileName = String(input.fileName)
      if (!/^[^/\\:]{1,120}$/.test(fileName) || typeof input.text !== 'string')
        throw new Error('下载参数无效')
      const url = URL.createObjectURL(new Blob([input.text], { type: 'text/plain;charset=utf-8' }))
      const link = document.createElement('a')
      link.href = url
      link.download = fileName
      link.click()
      setTimeout(() => URL.revokeObjectURL(url), 1000)
      return true
    }
    case 'backup-download': {
      if (bound?.id !== 'backup')
        throw new Error('下载未授权')
      const result = await request<{ url: string }>({ url: `/api/plugin-platform/call/backup/${bound.version}/backups.post.admin.settings.backups.download-url`, method: 'POST', data: { backupId: input.backupId } })
      const url = new URL(result.url)
      if (url.protocol !== 'https:')
        throw new Error('备份下载地址无效')
      const link = document.createElement('a')
      link.href = url.href
      link.rel = 'noopener noreferrer'
      link.click()
      return true
    }
    case 'external': {
      const url = String(input.url)
      if (!['client-access', 'keys'].includes(bound?.id ?? '') || !platformPlugins.value.some(p => p.id === 'client-access') || !url.startsWith('ccswitch://v1/import?'))
        throw new Error('外部跳转未授权')
      location.href = url
      return true
    }
    default: throw new Error('未知宿主操作')
  }
}
async function receive(event: MessageEvent) {
  const data = event.data
  const current = bound
  const replyToken = token
  if (event.source !== frame.value?.contentWindow || !current || data?.token !== replyToken || typeof data.id !== 'string' || data.id.length > 64 || typeof data.name !== 'string' || !/^[a-z][\w.-]{0,160}$/i.test(data.name))
    return
  const target = event.source as Window
  const respond = (value: unknown, failure?: string) => target.postMessage(JSON.parse(JSON.stringify({ token: replyToken, id: data.id, value, error: failure })), '*')
  if (pending >= 12) {
    respond(null, '操作繁忙')
    return
  }
  pending++
  try {
    if (JSON.stringify(data.input ?? {}).length > 8 * 1024 * 1024)
      throw new Error('请求过大')
    const value = data.type === 'call'
      ? await request({ url: `/api/plugin-platform/call/${current.id}/${current.version}/${data.name}`, method: 'POST', data: data.input ?? {}, silent: true })
      : data.type === 'host' ? await hostOperation(data.name, data.input ?? {}) : undefined
    respond(value)
  }
  catch (cause) {
    respond(null, cause instanceof Error ? cause.message : '操作失败')
  }
  finally {
    pending--
  }
}
function sendSettingsAction(action: 'save' | 'reset') {
  if (bound?.id !== 'gateway-settings' || props.active === false)
    return
  componentBridge?.events.dispatchEvent(new CustomEvent('plugin-settings-action', { detail: action }))
  frame.value?.contentWindow?.postMessage({ token, event: 'settings-action', action }, '*')
}
defineExpose({ sendSettingsAction })
// 目录轮询会替换对象，但相同页面不能重新加载，否则会清空表单与滚动位置。
watch([() => props.plugin.id, () => props.page.id], () => {
  void load()
}, { immediate: true,
})
watch(platformPlugins, () => {
  if (bound && !platformPlugins.value.some(p => p.id === bound?.id)) {
    html.value = ''
    componentRoot.value = undefined
    error.value = '插件已停用'
    releaseLease()
    emit('close')
  }
  sendContext()
})
watch(() => JSON.stringify(themeConfiguration()), sendContext)
watch(() => props.active, sendContext)
window.addEventListener('message', receive)
onScopeDispose(() => {
  releaseLease()
  generation++
  clearTimeout(readyTimer)
  window.removeEventListener('message', receive)
})
</script>

<template>
  <div ref="container" class="component-plugin flex min-h-0 flex-1 flex-col" :style="{ '--plugin-content-height': contentHeight }">
    <div v-if="error" role="alert" class="rounded-cp bg-cp-error-container p-4 text-cp-error-text">
      {{ error }} <button type="button" class="ml-3 underline" @click="load">
        重新打开
      </button>
    </div>
    <div v-if="componentRoot && !error && scrollable === false" class="flex min-h-0 flex-1 flex-col">
      <component :is="componentRoot" />
    </div>
    <BaseScrollbar v-else-if="componentRoot && !error" class="flex-1">
      <div :class="page.route ? ['component-page', { 'component-settings': page.layout === 'settings' }] : 'component-popup'">
        <component :is="componentRoot" />
      </div>
    </BaseScrollbar>
    <iframe v-if="html && !componentRoot" ref="frame" :srcdoc="html" sandbox="allow-scripts" referrerpolicy="no-referrer" :title="page.title" class="min-h-0 w-full flex-1 border-0 bg-cp-bg-layout" />
    <p v-if="!error && !html && !componentRoot" class="p-5 text-cp-text-secondary">
      正在加载插件…
    </p>
  </div>
</template>

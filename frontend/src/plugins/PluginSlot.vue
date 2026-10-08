<script setup lang="ts">
import type { PluginPage } from './catalog'
import { computed, onMounted, onScopeDispose, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { activePlugins, pluginPage, pluginRpc, refreshPlugins } from './catalog'

const props = defineProps<{ placement: PluginPage['slot'], pluginId?: string }>()
const pages = computed(() => activePlugins.value
  .filter(plugin => !props.pluginId || plugin.id === props.pluginId)
  .flatMap(plugin => plugin.pages.filter(page => page.slot === props.placement).map(page => ({ ...page, pluginId: plugin.id }))))
const selected = ref<(PluginPage & { pluginId: string }) | null>(null)
const open = ref(false)
const html = ref('')
const error = ref('')
const frame = ref<HTMLIFrameElement>()
let token = ''
let inFlight = 0
let serial = 0

async function show(page: NonNullable<typeof selected.value>) {
  const current = ++serial
  selected.value = page
  html.value = ''
  error.value = ''
  open.value = true
  token = crypto.randomUUID()
  try {
    const result = await pluginPage(page.pluginId, page.id)
    if (current !== serial || !open.value)
      return
    // opaque origin + CSP: 插件脚本只能通过下方显式 RPC 桥调用声明过的操作。
    const csp = `default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'`
    const bridge = `window.proxyPlugin={call:(name,input)=>new Promise((resolve,reject)=>{const id=crypto.randomUUID();const receive=e=>{if(e.source!==parent||e.data?.token!==${JSON.stringify(token)}||e.data?.id!==id)return;removeEventListener('message',receive);e.data.error?reject(new Error(e.data.error)):resolve(e.data.value)};addEventListener('message',receive);parent.postMessage({type:'plugin:rpc',token:${JSON.stringify(token)},id,name,input},'*')})}`
    html.value = `<meta http-equiv="Content-Security-Policy" content="${csp}"><script>${bridge}<\/script>${result.html}`
  }
  catch {
    if (current === serial)
      error.value = '插件页面暂不可用，请刷新状态后重试。'
  }
}

async function receive(event: MessageEvent) {
  const data = event.data
  const page = selected.value
  if (!open.value || !page || event.source !== frame.value?.contentWindow || data?.type !== 'plugin:rpc' || data.token !== token)
    return
  if (typeof data.id !== 'string' || data.id.length > 64 || typeof data.name !== 'string' || !/^[a-z][a-z0-9-]{0,63}$/.test(data.name))
    return
  const target = frame.value?.contentWindow
  const reply = { type: 'plugin:result', token, id: data.id }
  if (inFlight >= 4) {
    target?.postMessage({ ...reply, error: '请求繁忙，请稍后重试' }, '*')
    return
  }
  inFlight++
  try {
    if (JSON.stringify(data.input ?? null).length > 60000)
      throw new Error('payload')
    const value = await pluginRpc(page.pluginId, data.name, data.input ?? null)
    target?.postMessage({ ...reply, value }, '*')
  }
  catch {
    target?.postMessage({ ...reply, error: '插件调用失败或权限不足' }, '*')
  }
  finally {
    inFlight--
  }
}

watch([pages, open], () => {
  if (!open.value || (selected.value && !pages.value.some(page => page.id === selected.value?.id && page.pluginId === selected.value?.pluginId))) {
    open.value = false
    html.value = ''
    token = ''
    serial++
  }
})
onMounted(() => {
  void refreshPlugins()
  window.addEventListener('message', receive)
})
onScopeDispose(() => {
  serial++
  window.removeEventListener('message', receive)
})
</script>

<template>
  <div v-if="pages.length" class="flex flex-wrap gap-2">
    <BaseButton v-for="page in pages" :key="`${page.pluginId}/${page.id}`" variant="secondary" @click="show(page)">
      {{ page.title }}
    </BaseButton>
    <BaseModal v-model="open" :title="selected?.title || '插件'" size="lg">
      <p v-if="error" role="alert" class="text-cp-error-text">
        {{ error }}
      </p>
      <iframe v-else-if="html" ref="frame" :srcdoc="html" sandbox="allow-scripts" referrerpolicy="no-referrer" :title="selected?.title" class="h-[60vh] min-h-80 w-full rounded-cp border-0 bg-white" />
      <p v-else class="text-cp-text-secondary">
        正在加载…
      </p>
    </BaseModal>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, shallowRef, watch } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { buildProxyConnection, emptyProxyConnection, parseProxyConnection } from '@/utils/proxyConnection'

const props = defineProps<{ disabled: boolean, editing: boolean }>()
const value = defineModel<string>({ required: true })
const valid = defineModel<boolean>('valid', { required: true })
const mode = shallowRef(props.editing ? 'link' : 'form')
const fields = reactive(emptyProxyConnection())
const message = shallowRef('')
const showSecret = shallowRef(false)
const secretType = computed(() => showSecret.value ? 'text' : 'password')
const protocols = ['http', 'https', 'socks5', 'socks5h', 'vless', 'hysteria2'].map(value => ({ value, label: value === 'hysteria2' ? 'Hysteria2' : value.toUpperCase() }))
const securityOptions = computed(() => (fields.transport === 'ws' ? ['tls', 'none'] : ['reality', 'tls', 'none']).map(value => ({ value, label: { reality: 'Reality', tls: 'TLS', none: '无 TLS' }[value]! })))
const tunnel = computed(() => ['vless', 'hysteria2'].includes(fields.protocol))
const tls = computed(() => fields.protocol === 'hysteria2' || (fields.protocol === 'vless' && fields.security !== 'none'))
watch(mode, (next) => {
  message.value = ''
  if (next === 'form' && value.value) {
    try {
      Object.assign(fields, parseProxyConnection(value.value))
    }
    catch (error) {
      mode.value = 'link'
      message.value = error instanceof Error ? error.message : '无法解析链接'
    }
  }
})
watch(() => fields.transport, (transport) => {
  if (transport === 'ws') {
    fields.flow = ''
    if (fields.security === 'reality')
      fields.security = 'tls'
  }
})
watch(() => fields.protocol, (protocol) => {
  if (!fields.port)
    fields.port = ['vless', 'hysteria2', 'https'].includes(protocol) ? '443' : protocol.startsWith('socks') ? '1080' : '8080'
})
watch([mode, fields], () => {
  if (mode.value !== 'form') {
    valid.value = true
    return
  }
  try {
    value.value = buildProxyConnection(fields)
    valid.value = true
    message.value = ''
  }
  catch (error) {
    valid.value = false
    message.value = fields.host || fields.port ? (error instanceof Error ? error.message : '请检查连接参数') : ''
  }
}, { deep: true, immediate: true })
</script>

<template>
  <BaseSegmented v-model="mode" label="连接配置方式" :options="[{ label: '表单填写', value: 'form' }, { label: '粘贴链接', value: 'link' }]" :disabled="disabled" />
  <template v-if="mode === 'link'">
    <BaseFormItem label="代理链接" :required="!editing" :description="editing ? '留空保留当前连接和认证信息。填写新链接将替换连接。' : '支持 HTTP(S)、SOCKS5(H)、VLESS 和 Hysteria2 分享链接。'">
      <BaseInput v-model="value" :type="secretType" autocomplete="new-password" :disabled="disabled" placeholder="粘贴完整代理链接" aria-label="代理链接" />
    </BaseFormItem>
  </template>
  <template v-else>
    <BaseFormItem label="代理协议" required>
      <BaseSelect v-model="fields.protocol" class="w-full" :options="protocols" :disabled="disabled" />
    </BaseFormItem>
    <div class="grid grid-cols-1 gap-4 sm:grid-cols-[minmax(0,1fr)_8rem]">
      <BaseFormItem label="服务器地址" required>
        <BaseInput v-model="fields.host" :disabled="disabled" placeholder="域名或 IP" />
      </BaseFormItem>
      <BaseFormItem label="端口" required>
        <BaseInput v-model="fields.port" :disabled="disabled" inputmode="numeric" placeholder="443" />
      </BaseFormItem>
    </div>
    <template v-if="!tunnel">
      <BaseFormItem label="用户名（可选）">
        <BaseInput v-model="fields.username" :disabled="disabled" autocomplete="off" />
      </BaseFormItem>
      <BaseFormItem label="密码（可选）">
        <BaseInput v-model="fields.password" :disabled="disabled" :type="secretType" autocomplete="new-password" />
      </BaseFormItem>
    </template>
    <template v-if="fields.protocol === 'vless'">
      <BaseFormItem label="UUID" required>
        <BaseInput v-model="fields.uuid" :disabled="disabled" :type="secretType" autocomplete="new-password" />
      </BaseFormItem>
      <div class="grid grid-cols-2 gap-4">
        <BaseFormItem label="传输方式">
          <BaseSelect v-model="fields.transport" class="w-full" :options="[{ label: 'TCP', value: 'tcp' }, { label: 'WebSocket', value: 'ws' }]" :disabled="disabled" />
        </BaseFormItem>
        <BaseFormItem label="连接安全">
          <BaseSelect v-model="fields.security" class="w-full" :options="securityOptions" :disabled="disabled" />
        </BaseFormItem>
      </div>
      <BaseFormItem label="节点 UDP 设置" description="兼容分享链接设置；此网关出口用于 HTTPS / WSS，不提供 UDP 代理入口。">
        <BaseSelect v-model="fields.udp" class="w-full" :options="[{ label: '默认', value: '' }, { label: '允许', value: 'true' }, { label: '禁用', value: 'false' }]" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem v-if="fields.transport === 'tcp' && fields.security !== 'none'" label="Flow">
        <BaseSelect v-model="fields.flow" class="w-full" :options="[{ label: 'XTLS Vision', value: 'xtls-rprx-vision' }, { label: '无', value: '' }]" :disabled="disabled" />
      </BaseFormItem>
      <template v-if="fields.security === 'reality'">
        <BaseFormItem label="Reality 公钥" required>
          <BaseInput v-model="fields.publicKey" :disabled="disabled" />
        </BaseFormItem>
        <BaseFormItem label="Short ID">
          <BaseInput v-model="fields.shortId" :disabled="disabled" placeholder="节点提供的十六进制 Short ID" />
        </BaseFormItem>
      </template>
      <template v-if="fields.transport === 'ws'">
        <BaseFormItem label="WebSocket 路径">
          <BaseInput v-model="fields.path" :disabled="disabled" placeholder="/" />
        </BaseFormItem>
        <BaseFormItem label="WebSocket Host（可选）">
          <BaseInput v-model="fields.wsHost" :disabled="disabled" />
        </BaseFormItem>
      </template>
    </template>
    <template v-if="fields.protocol === 'hysteria2'">
      <BaseFormItem label="认证密码" required>
        <BaseInput v-model="fields.password" :disabled="disabled" :type="secretType" autocomplete="new-password" />
      </BaseFormItem>
      <BaseFormItem label="混淆">
        <BaseSelect v-model="fields.obfs" class="w-full" :options="[{ label: '无', value: '' }, { label: 'Salamander', value: 'salamander' }]" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem v-if="fields.obfs" label="混淆密码" required>
        <BaseInput v-model="fields.obfsPassword" :disabled="disabled" :type="secretType" autocomplete="new-password" />
      </BaseFormItem>
    </template>
    <template v-if="tls">
      <BaseFormItem label="SNI / 服务器名称" :required="fields.security === 'reality' && fields.protocol === 'vless'">
        <BaseInput v-model="fields.sni" :disabled="disabled" placeholder="留空使用服务器地址（Reality 必填）" />
      </BaseFormItem>
      <BaseFormItem v-if="fields.protocol === 'vless'" label="TLS 指纹">
        <BaseSelect v-model="fields.fingerprint" class="w-full" :options="['chrome', 'firefox', 'safari', 'ios', 'android', 'edge', '360', 'qq', 'random', 'randomized'].map(value => ({ label: value, value }))" :disabled="disabled" />
      </BaseFormItem>
      <BaseFormItem label="ALPN（可选）">
        <BaseInput v-model="fields.alpn" :disabled="disabled" placeholder="按节点填写，例如 h2,http/1.1" />
      </BaseFormItem>
      <BaseSwitch v-model="fields.insecure" label="跳过证书验证（仅节点明确要求时开启）" show-label :disabled="disabled" />
    </template>
  </template>
  <p v-if="message" role="status" class="m-0 text-cp-sm text-cp-warning-text">
    {{ message }}
  </p>
  <BaseSwitch v-model="showSecret" label="显示连接凭据" show-label :disabled="disabled" />
</template>

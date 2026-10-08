<script setup lang="ts">
import type { OutboundProxyRecord, RequestLocation } from '@/api'
import { Save, Wifi } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import RequestLocationFields from '@/components/RequestLocationFields.vue'
import ProxyConnectionFields from './ProxyConnectionFields.vue'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
  saving: boolean
  testing: boolean
}>()
const emit = defineEmits<{
  save: []
  test: []
}>()
const open = defineModel<boolean>({ required: true })
const name = defineModel<string>('name', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const customLocation = defineModel<boolean>('customLocation', { required: true })
const location = defineModel<RequestLocation>('location', { required: true })
const connectionValid = shallowRef(true)
const busy = computed(() => props.saving || props.testing)
const title = computed(() => props.proxy ? '编辑代理' : '新增代理')
</script>

<template>
  <BaseModal v-model="open" :title="title" size="lg" :dismissible="!busy">
    <BaseForm class="grid gap-5">
      <BaseFormItem label="代理名称" required>
        <BaseInput v-model="name" maxlength="100" :disabled="busy" aria-label="代理名称" placeholder="请输入代理名称" />
      </BaseFormItem>
      <ProxyConnectionFields v-if="open" v-model="proxyUrl" v-model:valid="connectionValid" :editing="!!proxy" :disabled="busy" />
      <BaseSwitch v-model="customLocation" label="自定义时区位置" show-label :disabled="busy" />
      <RequestLocationFields v-if="customLocation" v-model="location" :disabled="busy" />
      <p v-if="proxy?.accountCount && proxyUrl.trim()" class="m-0 text-cp-sm text-cp-warning-text">
        将更新 {{ proxy.accountCount }} 个关联账号的出口
      </p>
    </BaseForm>
    <template #footer>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="secondary" :loading="testing" :disabled="saving || !connectionValid" @click="emit('test')">
        <template #icon>
          <Wifi class="size-4" />
        </template>
        测试连接
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="testing || !connectionValid" @click="emit('save')">
        <template #icon>
          <Save class="size-4" />
        </template>
        保存代理
      </BaseButton>
    </template>
  </BaseModal>
</template>

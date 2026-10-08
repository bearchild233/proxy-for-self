<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'

const maxRequests = defineModel<string>('maxRequests', { required: true })
const maxBodyMiB = defineModel<string>('maxBodyMiB', { required: true })
const bodyBudgetMiB = defineModel<string>('bodyBudgetMiB', { required: true })
</script>

<template>
  <BaseCard title="网关容量" description="保存后用于新请求，正在进行的请求继续完成。其他设备共享，重启后保留。">
    <BaseForm class="max-w-6xl sm:grid-cols-3 sm:items-end">
      <BaseFormItem label="全网关并发上限" description="所有账号合计；账号和密钥限制仍生效，0 表示不限">
        <BaseInput v-model="maxRequests" aria-label="全网关并发上限" type="number" min="0" max="65535" step="1" />
      </BaseFormItem>
      <BaseFormItem label="单请求正文上限" description="包含历史上下文、图片等请求内容">
        <BaseInput v-model="maxBodyMiB" aria-label="单请求正文上限" type="number" min="1" max="4095" step="1">
          <template #suffix>
            MiB
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem label="在途正文总预算" description="同时处理的正文预算，不等于进程内存；0 表示不限">
        <BaseInput v-model="bodyBudgetMiB" aria-label="在途正文总预算" type="number" min="0" max="4095" step="1">
          <template #suffix>
            MiB
          </template>
        </BaseInput>
      </BaseFormItem>
    </BaseForm>
    <p class="mb-0 mt-3 text-cp-sm text-cp-muted">
      调低预算会等待在途请求释放占用。已有 WebSocket 连接调大正文上限后需重新连接。
    </p>
  </BaseCard>
</template>

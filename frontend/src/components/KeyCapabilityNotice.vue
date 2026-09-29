<script setup lang="ts">
import { computed } from 'vue'
import { keyCapabilities } from '@/utils/keyCapabilities'

const props = defineProps<{ excelEnabled: boolean }>()
const info = computed(() => keyCapabilities(props.excelEnabled))
</script>

<template>
  <section aria-label="当前 Key 的能力与限制" class="space-y-2 text-cp-xs text-cp-text-secondary">
    <p class="m-0 font-emphasis text-cp-text">
      {{ info.channel }} · {{ info.summary }}
    </p>
    <p v-if="excelEnabled" class="m-0">
      图片失败会按原桥降级为说明文本 · 不支持文件输入与 previous_response_id
    </p>
    <details class="rounded-cp-card bg-cp-fill-quaternary p-3">
      <summary class="cursor-pointer font-emphasis text-cp-text">
        查看接口、图片行为与会话限制
      </summary>
      <dl class="mt-3 mb-0 grid gap-2">
        <div v-for="[label, value] in info.rows" :key="label" class="grid gap-1 sm:grid-cols-[6rem_1fr]">
          <dt class="font-emphasis text-cp-text">
            {{ label }}
          </dt>
          <dd class="m-0 wrap-anywhere">
            {{ value }}
          </dd>
        </div>
      </dl>
      <ul class="mt-3 mb-0 space-y-2 pl-4">
        <li v-for="warning in info.warnings" :key="warning">
          {{ warning }}
        </li>
      </ul>
      <p class="mt-3 mb-0">
        能力说明不代表当前账号已完成真实上游验收
      </p>
    </details>
  </section>
</template>

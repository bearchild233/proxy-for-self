<script setup lang="ts">
import type { UsageDisplayRecord } from '../utils/records'

import { Bot, SquareDashedBottomCode, Zap } from '@lucide/vue'
import { usageIsFast, usageIsReview, usageIsSubagent, usageReasoningEffort } from '../utils/records'

defineProps<{
  record: UsageDisplayRecord
}>()
</script>

<template>
  <span
    class="inline-flex items-center gap-1 whitespace-nowrap text-cp-sm font-bold text-cp-text"
  >
    <span>{{ usageReasoningEffort(record) }}</span>
    <span
      v-if="usageIsSubagent(record)"
      class="inline-flex shrink-0 text-cp-purple-text"
      title="子代理请求"
      aria-label="子代理请求"
    >
      <Bot class="size-3.5" stroke-width="2.2" />
    </span>
    <span
      v-if="usageIsReview(record)"
      class="inline-flex shrink-0 text-cp-purple-text"
      title="Review 子代理请求"
      aria-label="Review 子代理请求"
    >
      <SquareDashedBottomCode class="size-3.25" stroke-width="2.2" />
    </span>
    <span
      v-if="usageIsFast(record)"
      class="inline-flex shrink-0 text-cp-warning-text"
      title="Fast 请求：最终发送档位为 Fast / Priority，本地费用按该档位估算；上游回传档位见详情。"
      aria-label="Fast 请求"
    >
      <Zap class="size-3.5" stroke-width="2.2" />
    </span>
  </span>
</template>

<script setup lang="ts">
import { usePluginPolling } from '@sdk/polling'
import { call } from '@sdk/ui'
import { computed, onMounted, ref } from 'vue'
import { formatDateTime } from '@/utils/date'

const emit = defineEmits<{ synced: [] }>()
const state = ref<{ status?: string, lastSuccess?: number, nextRun?: number, checkedAt?: number, error?: string }>({})
const failure = ref('')
const healthy = computed(() => state.value.checkedAt && Date.now() / 1000 - state.value.checkedAt < 180)
const format = (value?: number) => value ? formatDateTime(value * 1000) : '—'
async function refresh() {
  try {
    const result = await call<typeof state.value>('pricing.schedule.status', {})
    if (state.value.lastSuccess && result.lastSuccess !== state.value.lastSuccess)
      emit('synced')
    state.value = result
    failure.value = ''
  }
  catch { failure.value = '同步状态读取失败' }
}
onMounted(refresh)
usePluginPolling(refresh, ref(30_000))
</script>

<template>
  <div class="mb-3 flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 text-cp-xs text-cp-text-secondary" aria-label="价格自动同步状态">
    <span>每日 03:00 自动同步 · 保留自定义价格与倍率</span>
    <span v-if="state.status === 'running' && healthy">同步中…</span>
    <span v-else-if="!healthy">调度器尚未就绪</span>
    <span>最近成功：{{ format(state.lastSuccess) }}</span>
    <span v-if="state.nextRun">下次：{{ format(state.nextRun) }}</span>
    <span v-if="failure || state.error" role="status" class="text-cp-error-text">{{ failure || state.error }}</span>
  </div>
</template>

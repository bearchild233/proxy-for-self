<script setup lang="ts">
import type { WarmupRead, WarmupRun, WarmupRunInput } from '@/api/modules/accountWarmup'
import { ChevronDown, Flame, Settings2 } from '@lucide/vue'
import { enabled } from '@sdk/catalog'
import { usePluginPolling } from '@sdk/polling'
import { host } from '@sdk/ui'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { readAccountWarmup, runAccountWarmup } from '@/api/modules/accountWarmup'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BasePopover from '@/components/base/BasePopover.vue'

const props = defineProps<{ selectedAccountIds: string[], archived: boolean }>()
const view = shallowRef<WarmupRead>()
const error = ref('')
const submitting = ref(false)
const result = shallowRef<WarmupRun>()
const installed = computed(() => enabled('account-warmup'))
const ready = computed(() => installed.value && view.value?.available === true && view.value.runtime.ready)
const status = computed(() => {
  if (props.archived)
    return '归档账号不参与预热'
  if (!installed.value || (view.value?.available === false && view.value.reason === 'plugin_disabled'))
    return '预热插件未安装或未启用'
  if (error.value)
    return error.value
  if (!ready.value)
    return '预热服务尚未就绪'
  const run = result.value ?? (view.value?.available ? view.value.runtime.lastRun : null)
  return run ? `${run.status === 'completed' ? '最近预热已结束' : run.status === 'cancelled' ? '最近预热已停止' : '已加入预热队列'}：收头 ${run.accepted} / ${run.total}，失败 ${run.failed}，跳过 ${run.skipped}，未知 ${run.unknown}` : '向全部或选中账号发送一次 hi'
})
let submission: { key: string, input: WarmupRunInput } | undefined
let readSequence = 0
async function refresh() {
  const sequence = ++readSequence
  if (!installed.value) {
    view.value = undefined
    return
  }
  try {
    const next = await readAccountWarmup()
    if (sequence === readSequence) {
      view.value = next
      if (next.available && next.runtime.lastRun?.id === result.value?.id)
        result.value = next.runtime.lastRun ?? undefined
      error.value = ''
    }
  }
  catch (cause) {
    if (sequence === readSequence)
      error.value = cause instanceof Error ? cause.message : '预热状态读取失败'
  }
}
async function run(scope: 'all' | 'selected') {
  if (!ready.value || props.archived || submitting.value)
    return
  const ids = scope === 'selected' ? [...new Set(props.selectedAccountIds)] : undefined
  if (ids && (!ids.length || ids.length > 500)) {
    error.value = '请选择 1 至 500 个账号'
    return
  }
  const key = JSON.stringify([scope, ids])
  // 网络结果未知时重用原提交标识；管理员明确再点已完成批次时才创建新标识。
  if (submission?.key !== key)
    submission = { key, input: { submissionId: crypto.randomUUID(), scope, ...(ids ? { accountIds: ids } : {}) } }
  submitting.value = true
  error.value = ''
  try {
    const next = await runAccountWarmup(submission.input)
    result.value = next.run
    submission = undefined
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '预热提交失败' }
  finally { submitting.value = false }
}
async function settings() {
  try {
    await host('navigate', { path: '/extensions/account-warmup' })
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '预热设置暂不可用' }
}
onMounted(refresh)
watch(installed, () => {
  void refresh()
})
usePluginPolling(refresh, shallowRef(15_000))
</script>

<template>
  <div class="flex flex-col items-end gap-1" :title="status">
    <div class="flex items-center gap-1.5">
      <BaseButton variant="secondary" :loading="submitting" :disabled="!ready || archived || selectedAccountIds.length > 500" @click="run(selectedAccountIds.length ? 'selected' : 'all')">
        <Flame class="size-4" /> {{ selectedAccountIds.length ? `预热选中 (${selectedAccountIds.length})` : '预热全部账号' }}
      </BaseButton>
      <BasePopover placement="bottom-end">
        <template #trigger="{ open }">
          <BaseIconButton label="选择预热范围" :pressed="open" :disabled="!ready || archived || submitting">
            <ChevronDown class="size-4" />
          </BaseIconButton>
        </template>
        <template #default="{ close }">
          <div class="w-52 p-1.5">
            <BaseMenuItem :disabled="!ready || archived || submitting" @click="close(); run('all')">
              预热全部当前账号
            </BaseMenuItem>
            <BaseMenuItem :disabled="!ready || archived || submitting || !selectedAccountIds.length || selectedAccountIds.length > 500" @click="close(); run('selected')">
              预热选中 ({{ selectedAccountIds.length }})
            </BaseMenuItem>
            <p class="mx-2 my-1 text-cp-xs text-cp-text-secondary">
              全部包含当前账号池，不受列表筛选或页码影响。
            </p>
          </div>
        </template>
      </BasePopover>
      <BaseIconButton label="预热设置" :disabled="!installed" @click="settings">
        <Settings2 class="size-4" />
      </BaseIconButton>
    </div>
    <span v-if="error || !ready || archived || result" role="status" class="max-w-96 text-right text-cp-xs" :class="error ? 'text-cp-error-text' : 'text-cp-text-secondary'">{{ status }}</span>
  </div>
</template>

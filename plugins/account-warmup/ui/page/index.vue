<script setup lang="ts">
import type { WarmupOverride, WarmupReasoningEffort } from '@/api/modules/accountWarmup'
import { ArrowLeft, Clock3, RefreshCw, Save } from '@lucide/vue'
import { host } from '@sdk/ui'
import { computed, ref, watch } from 'vue'
import { warmupEffortOptions, warmupRequestError } from '@/api/modules/accountWarmup'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import WarmupAccountRow from './components/WarmupAccountRow.vue'
import WarmupHistory from './components/WarmupHistory.vue'
import { formatWarmupTime } from './model'
import { useWarmupSettings } from './useWarmupSettings'

const { view, draft, dirty, validation, error, namesError, saveError, changedElsewhere, loading, saving, names, runs, items, selectedRunId, historyError, cancelling, models, modelLoading, modelErrors, refresh, refreshNames, save, refreshHistory, selectRun, cancel, loadModels } = useWarmupSettings()
const search = ref('')
const page = ref(1)
const catalogAccountId = ref('')
const available = computed(() => view.value?.available === true)
const accounts = computed(() => view.value?.available ? view.value.accounts : [])
const runtime = computed(() => view.value?.available ? view.value.runtime : undefined)
const savedEnabled = computed(() => view.value?.available && view.value.settings.enabled)
const promptBytes = computed(() => new TextEncoder().encode(draft.value.prompt).length)
const promptError = computed(() => warmupRequestError({ prompt: draft.value.prompt, reasoningEffort: 'auto' }))
const filtered = computed(() => {
  const query = search.value.trim().toLowerCase()
  return accounts.value.filter(account => `${names.value.get(account.id)?.name ?? ''} ${account.id} ${account.provider}`.toLowerCase().includes(query))
})
const pageCount = computed(() => Math.max(1, Math.ceil(filtered.value.length / 20)))
const currentPage = computed(() => Math.min(page.value, pageCount.value))
const visibleAccounts = computed(() => filtered.value.slice((currentPage.value - 1) * 20, currentPage.value * 20))
const catalogOptions = computed(() => [{ value: '', label: '选择账号以读取模型目录' }, ...accounts.value.filter(item => item.supported).map(item => ({ value: item.id, label: names.value.get(item.id)?.name || item.id }))])
const modelOptions = computed(() => {
  const entries = new Map((models.value[catalogAccountId.value] ?? []).map(item => [item.id, { value: item.id, label: item.label || item.id }]))
  if (draft.value.defaultModel && !entries.has(draft.value.defaultModel))
    entries.set(draft.value.defaultModel, { value: draft.value.defaultModel, label: `${draft.value.defaultModel}（已设置）` })
  return [...entries.values()]
})
const effortOptions = computed(() => warmupEffortOptions(models.value[catalogAccountId.value]?.find(item => item.id === draft.value.defaultModel), draft.value.reasoningEffort))
function setEffort(value: string) {
  if (effortOptions.value.some(item => item.value === value && !item.disabled))
    draft.value.reasoningEffort = value as WarmupReasoningEffort
}
function selectAccount(id: string, selected: boolean) {
  const next = new Set(draft.value.accountIds)
  if (selected)
    next.add(id)
  else next.delete(id)
  draft.value.accountIds = [...next]
}
function updateOverride(value: WarmupOverride) {
  draft.value.accountOverrides = [...draft.value.accountOverrides.filter(item => item.accountId !== value.accountId), value]
}
function setCatalog(id: string) {
  catalogAccountId.value = id
  if (id)
    void loadModels(id, draft.value.defaultModel)
}
watch(accounts, (value) => {
  if (!value.some(item => item.id === catalogAccountId.value && item.supported))
    setCatalog(value.find(item => item.supported)?.id ?? '')
}, { immediate: true })
async function back() {
  try {
    await host('navigate', { path: '/accounts' })
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '无法返回账号管理' }
}
</script>

<template>
  <div class="flex w-full flex-col gap-4 pb-6">
    <BasePageHeader title="自动预热" description="管理后台自动化计划；手动预热请返回账号管理。">
      <template #actions>
        <BaseButton variant="secondary" @click="back">
          <ArrowLeft class="size-4" /> 返回账号管理
        </BaseButton>
        <BaseButton :disabled="saving || loading || !available" @click="refresh(true)">
          撤销
        </BaseButton>
        <BaseButton variant="primary" :loading="saving" :disabled="!available || loading || !dirty || Boolean(validation)" @click="save">
          <Save class="size-4" /> 保存设置
        </BaseButton>
      </template>
    </BasePageHeader>
    <p class="m-0 rounded-cp bg-cp-primary-container px-4 py-3 text-cp-sm leading-relaxed text-cp-primary-on-container">
      本页设置每日计划和额度重置后的自动任务，关闭浏览器后仍会执行。每次发送一条预热请求，仅收取响应头；上游受理不代表已开启 5 小时计时，只有真实额度观测才能确认。支持 OpenAI OAuth，沿用账号出口、额度和养号保护。
    </p>
    <div v-if="error || (view && !view.available)" role="alert" class="rounded-cp bg-cp-error-container p-4 text-cp-sm text-cp-error-text">
      {{ error || (view && !view.available && view.reason === 'plugin_disabled' ? '预热插件未安装或未启用，请在插件管理中启用。' : '预热服务尚未就绪，请稍后刷新。') }}
      <BaseButton class="ml-3" size="sm" :loading="loading" @click="refresh()">
        重试
      </BaseButton>
    </div>
    <p v-if="loading && !view" role="status" class="m-0 p-4 text-cp-text-secondary">
      正在读取预热设置…
    </p>
    <div v-if="changedElsewhere" role="alert" class="flex flex-wrap items-center gap-3 rounded-cp bg-cp-warning-container p-4 text-cp-sm text-cp-warning-text">
      <span class="flex-1">设置已被其他页面修改。当前草稿已保留，请加载最新配置后重新调整。</span>
      <BaseButton size="sm" :disabled="saving" @click="refresh(true)">
        加载最新并替换草稿
      </BaseButton>
    </div>
    <p v-if="saveError" role="alert" class="m-0 text-cp-sm text-cp-error-text">
      保存失败：{{ saveError }}。未保存修改已保留。
    </p>
    <template v-if="available">
      <BaseCard title="自动预热" description="计划由插件后台执行，关闭浏览器后仍继续生效。">
        <template #actions>
          <BaseSwitch v-model="draft.enabled" label="启用自动预热" show-label :disabled="saving" />
        </template>
        <div class="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
          <BaseFormItem label="额度重置触发" description="仅依据已观测到的真实窗口到期时间。">
            <BaseSwitch v-model="draft.resetEnabled" label="额度重置后预热" show-label :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="重置后延迟（秒）">
            <BaseInput :model-value="String(draft.resetDelaySeconds)" type="number" min="0" max="86400" aria-label="全局重置后延迟秒数" :disabled="saving" @update:model-value="draft.resetDelaySeconds = $event === '' ? Number.NaN : Number($event)" />
          </BaseFormItem>
          <BaseFormItem label="每日定时触发">
            <BaseSwitch v-model="draft.dailyEnabled" label="每天定时预热" show-label :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="每日执行时间">
            <BaseInput v-model="draft.dailyTime" type="time" aria-label="默认每日执行时间" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="调度时区" description="每日计划与账号独立时间共用此时区。">
            <BaseInput v-model="draft.timezone" aria-label="调度时区" placeholder="Asia/Shanghai" :disabled="saving" />
          </BaseFormItem>
          <BaseFormItem label="随机延迟（秒）" description="每次计划在范围内取值，排队后保持该延迟。">
            <div class="flex items-center gap-2">
              <BaseInput :model-value="String(draft.jitterMinSeconds)" type="number" min="0" max="300" aria-label="随机延迟最小秒数" :disabled="saving" @update:model-value="draft.jitterMinSeconds = $event === '' ? Number.NaN : Number($event)" /><span>至</span><BaseInput :model-value="String(draft.jitterMaxSeconds)" type="number" min="0" max="300" aria-label="随机延迟最大秒数" :disabled="saving" @update:model-value="draft.jitterMaxSeconds = $event === '' ? Number.NaN : Number($event)" />
            </div>
          </BaseFormItem>
        </div>
        <p class="mt-4 mb-0 flex flex-wrap items-center gap-x-4 gap-y-1 text-cp-xs text-cp-text-secondary" role="status">
          <span><Clock3 class="mr-1 inline size-3.5" /> {{ savedEnabled ? (runtime?.ready ? '自动预热已开启' : '服务尚未就绪') : '自动预热已关闭' }}{{ dirty ? ' · 草稿待保存' : '' }}</span><span>下次自动任务：{{ formatWarmupTime(runtime?.nextAutomaticRunAt) }}</span><span>最近自动批次：{{ formatWarmupTime(runs[0]?.createdAt) }}</span>
        </p>
      </BaseCard>
      <BaseCard title="自动预热请求" description="提示词、默认模型和推理强度仅用于后台自动任务，不修改账号管理中手动预热的本次参数。">
        <template #actions>
          <BaseButton v-if="catalogAccountId" size="sm" :loading="modelLoading.has(catalogAccountId)" :disabled="saving" @click="loadModels(catalogAccountId, draft.defaultModel)">
            <RefreshCw class="size-3.5" /> 刷新模型目录
          </BaseButton>
        </template>
        <BaseFormItem label="预热提示词" :description="`最多 4096 字节，可使用多行文本；当前 ${promptBytes} / 4096 字节。`" :error="promptError">
          <BaseTextarea v-model="draft.prompt" :rows="4" aria-label="自动预热提示词" placeholder="hi" :disabled="saving" />
        </BaseFormItem>
        <div class="mt-4 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <BaseFormItem label="读取账号模型目录">
            <BaseSelect :model-value="catalogAccountId" searchable :options="catalogOptions" aria-label="模型目录账号" :disabled="saving" @update:model-value="setCatalog" />
          </BaseFormItem>
          <BaseFormItem label="默认自动预热模型">
            <BaseSelect v-model="draft.defaultModel" searchable :options="modelOptions" aria-label="选择默认预热模型" :disabled="saving || modelLoading.has(catalogAccountId)" />
          </BaseFormItem>
          <BaseFormItem label="默认推理强度" description="自动会选择该模型支持的最低强度。">
            <BaseSelect :model-value="draft.reasoningEffort" :options="effortOptions" aria-label="自动预热默认推理强度" :disabled="saving" @update:model-value="setEffort" />
          </BaseFormItem>
        </div>
        <p v-if="catalogAccountId && modelErrors[catalogAccountId]" role="alert" class="mt-3 mb-0 text-cp-xs text-cp-error-text">
          {{ modelErrors[catalogAccountId] }}
        </p>
        <p class="mt-3 mb-0 text-cp-xs text-cp-text-secondary">
          目录属于所选账号，各账号实际权限和支持的强度可能不同；可在下方覆盖各账号的模型与强度。目录不可用时保留已保存的模型，执行前由服务逐项校验。
        </p>
      </BaseCard>
      <BaseCard title="参与账号" description="这里只选择后台自动计划的目标账号，账号管理中的手动操作使用本次选择的范围。">
        <template #actions>
          <span class="text-cp-sm text-cp-text-secondary">指定 {{ draft.accountIds.length }} 个 · 目录 {{ accounts.length }} 个</span>
        </template>
        <div class="flex flex-wrap items-center gap-3">
          <BaseSegmented v-model="draft.targetMode" class="max-w-full" label="自动预热账号范围" :options="[{ label: '全部当前账号', value: 'all' }, { label: '指定账号', value: 'selected' }]" :disabled="saving" />
          <BaseInput v-model="search" class="min-w-48 flex-1" aria-label="搜索预热账号" placeholder="搜索账号名称或 ID" @update:model-value="page = 1" />
          <BaseButton size="sm" @click="refreshNames">
            <RefreshCw class="size-3.5" /> 账号名称
          </BaseButton>
        </div>
        <p v-if="namesError" role="status" class="mt-3 mb-0 text-cp-xs text-cp-warning-text">
          {{ namesError }}
        </p>
        <p class="my-3 text-cp-xs text-cp-text-secondary">
          停用、归档、不支持或受额度保护的账号会在执行前跳过；展开账号可设置独立时间、模型、推理强度和重置延迟。
        </p>
        <div class="grid gap-2">
          <WarmupAccountRow v-for="account in visibleAccounts" :key="account.id" :account="account" :name="names.get(account.id)?.name || account.id" :settings="draft" :selected="draft.accountIds.includes(account.id)" :disabled="saving" :models="models[account.id] ?? []" :models-loading="modelLoading.has(account.id)" :models-error="modelErrors[account.id] ?? ''" @select="selectAccount(account.id, $event)" @override="updateOverride" @models="loadModels" />
        </div>
        <p v-if="!visibleAccounts.length" class="my-4 text-center text-cp-sm text-cp-text-secondary">
          暂无匹配账号
        </p>
        <div v-if="pageCount > 1" class="mt-3 flex items-center justify-end gap-3">
          <BaseButton size="sm" :disabled="currentPage <= 1" @click="page = currentPage - 1">
            上一页
          </BaseButton><span class="text-cp-xs text-cp-text-secondary">{{ currentPage }} / {{ pageCount }}</span><BaseButton size="sm" :disabled="currentPage >= pageCount" @click="page = currentPage + 1">
            下一页
          </BaseButton>
        </div>
      </BaseCard>
      <p v-if="validation && dirty" role="alert" class="m-0 text-cp-sm text-cp-error-text">
        {{ validation }}
      </p>
      <WarmupHistory :runs="runs" :items="items" :selected-id="selectedRunId" :names="names" :error="historyError" :cancelling="cancelling" @select="selectRun" @refresh="refreshHistory" @cancel="cancel" />
    </template>
  </div>
</template>

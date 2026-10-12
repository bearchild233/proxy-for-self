<script setup lang="ts">
import { usePluginPolling } from '@sdk/polling'
import { call } from '@sdk/ui'
import { computed, onMounted, ref } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import { formatDateTime } from '@/utils/date'

interface Settings {
  mode: 'lazy' | 'continuous'
  windowSeconds: number
  initialRestSeconds: number
  maximumRestSeconds: number
  probeIntervalMs: number
  groupIds: string[]
  gateways: string[]
  poolAccountIds: Record<string, string>
  poolUrl: string
  poolTokenConfigured?: boolean
}
interface Pair {
  accountId: string
  gateway: string
  state: string
  restUntil?: number
  restSeconds?: number
  learnedRestSeconds?: number
  inFlight: number
  firstContactAt?: number
}
interface Observation {
  id: number
  at: number
  account: string
  gateway: string
  source: string
  status?: number
  classification?: string
  plannedRestSeconds?: number
  observedIdleSeconds?: number
  durationMs?: number
  phase?: string
  stateReturned?: boolean
  retry?: number
}
interface Experiment { mode?: string, conversationMode?: string, accountName?: string, requestSha256?: string, fixedSession?: boolean, fixedCacheKey?: boolean, status: string, elapsedSeconds: number, remainingSeconds: number, fetchedAt: number, businessCount: number, normalCount: number, degradedCount: number, errorCount: number, switchCount: number, candidateCount: number }
interface CacheSample { phase: string, gateway?: string, datacenterCountry?: string, messageCount?: number, inputTokens?: number, cachedTokens?: number, outputTokens?: number, cacheWriteTokens?: number, durationMs?: number, classification?: string, stateReturned?: boolean, result: string }
interface View { preview?: boolean, readOnly?: boolean, cacheSamples?: CacheSample[], experiment?: Experiment, settings: Settings, pairs: Pair[], observations: Observation[], switches: { at: number, accountId: string, fromGateway?: string, toGateway: string, previousWindowSeconds?: number, preparationSeconds: number }[] }
interface Account { id: string, name: string, email?: string, provider?: string, groups: { id: string }[] }
const view = ref<View>()
const draft = ref<Settings>()
const groups = ref<{ id: string, name: string }[]>([])
const accounts = ref<Account[]>([])
const token = ref('')
const gatewayText = ref('')
const initialHours = ref(1)
const maximumHours = ref(24)
const tab = ref('pairs')
const accountFilter = ref('')
const gatewayFilter = ref('')
const error = ref('')
const saving = ref(false)
const dirty = ref(false)
const refreshInterval = ref(30_000)
const eligible = computed(() => accounts.value.filter(a => a.groups?.some(g => draft.value?.groupIds.includes(g.id))))
const names = computed(() => new Map(accounts.value.map(a => [a.id, a.name || a.email || a.id])))
const duration = (seconds?: number) => seconds == null ? '—' : seconds >= 3600 ? `${(seconds / 3600).toFixed(1).replace(/\.0$/, '')}h` : `${Math.round(seconds)}s`
const timestamp = (seconds?: number) => seconds ? formatDateTime(seconds * 1000) : '—'
const states: Record<string, string> = { unknown: '待检测', preparing: '准备中', ready: '可用', draining: '排空中', cooling: '静置中' }
const source = (sample: Observation) => sample.source === 'business' ? '业务' : sample.source === 'probe' ? '判定' : `${sample.phase === 'bootstrap' ? '取材料' : '重放检测'}${sample.retry ? ' · 重试' : ''}`
async function refresh() {
  try {
    view.value = await call<View>('gateway-pool.read', { accountId: accountFilter.value || undefined, gateway: gatewayFilter.value || undefined, limit: 100 })
    refreshInterval.value = view.value.experiment ? 5_000 : 30_000
    if (!dirty.value) {
      draft.value = JSON.parse(JSON.stringify(view.value.settings)) as Settings
      initialHours.value = draft.value.initialRestSeconds / 3600
      maximumHours.value = draft.value.maximumRestSeconds / 3600
      gatewayText.value = draft.value.gateways.join('\n')
    }
    error.value = ''
  }
  catch (e) { error.value = e instanceof Error ? e.message : '读取网关池失败' }
}
async function loadDirectory() {
  // 目录分页读取，不把首屏误当作全部可选账号/分组。
  function unpack<T>(result: { data?: T } | T): T {
    return (result && typeof result === 'object' && 'data' in result ? result.data : result) as T
  }
  async function collect<T>(operation: string) {
    const items: T[] = []
    for (let page = 1; ; page++) {
      const result = unpack(await call<{ items: T[], page: { totalPages: number } }>(operation, { page, pageSize: 100 }))
      items.push(...result.items)
      if (page >= result.page.totalPages)
        return items
    }
  }
  try {
    [groups.value, accounts.value] = await Promise.all([collect<{ id: string, name: string }>('account-groups.get.admin.account-groups'), collect<Account>('accounts.get.admin.accounts')])
  }
  catch { error.value = '账号或分组目录读取失败' }
}
async function save() {
  if (!draft.value || saving.value)
    return
  saving.value = true
  try {
    const mappings = Object.fromEntries(eligible.value.map(a => [a.id, draft.value!.poolAccountIds[a.id] || '']).filter(([, id]) => id))
    await call('gateway-pool.configure', {
      ...draft.value,
      poolTokenConfigured: undefined,
      poolToken: token.value || undefined,
      poolAccountIds: mappings,
      initialRestSeconds: initialHours.value * 3600,
      maximumRestSeconds: maximumHours.value * 3600,
      gateways: gatewayText.value.split(/[\s,;]+/).filter(Boolean),
    })
    token.value = ''
    dirty.value = false
    await refresh()
  }
  catch (e) { error.value = e instanceof Error ? e.message : '保存失败' }
  finally { saving.value = false }
}
onMounted(() => {
  void refresh()
  void loadDirectory()
})
usePluginPolling(refresh, refreshInterval)
</script>

<template>
  <section class="pool-page">
    <header class="pool-header">
      <div><h1>网关池</h1><p>astra · 固定配套材料 · 账号与网关独立退避</p></div>
      <BaseButton v-if="!view?.readOnly" :loading="saving" :disabled="!draft || !dirty" @click="save">
        保存设置
      </BaseButton>
    </header>
    <p v-if="view?.preview" class="pool-note">
      本机模拟数据预览，不调用真实账号或上游。
    </p>
    <p v-if="error" role="alert" class="pool-error">
      {{ error }}
    </p>
    <div v-if="view?.experiment" class="pool-card pool-experiment" aria-live="polite">
      <div class="pool-experiment-heading">
        <strong>{{ view.experiment.accountName || 'yxx plus' }} · {{ view.experiment.mode === 'cache' ? '换网关缓存实验' : '真实保活实验' }}</strong><span>{{ view.experiment.status === 'running' ? '运行中' : view.experiment.status === 'failed' ? '实验失败' : view.experiment.status === 'prepared' ? '准备完成，尚未请求' : '已结束' }}</span>
      </div>
      <p class="pool-note">
        {{ view.experiment.mode === 'cache' ? (view.experiment.conversationMode === 'append' ? 'astra low · 背景资料 + 新问题 + 真实回答逐轮追加 · 固定 session id 和缓存 key · 6 个网关各 3 次' : 'astra low · 固定完整输入、session id 和缓存 key · 读取完整 SSE usage · 多网关切换与回切对照') : '每 10s 发送 hi · astra low · 响应头后关闭 SSE · 最长 2h／全部静置提前结束' }}
      </p>
      <div class="pool-experiment-metrics">
        <div><span>已运行 / 剩余</span><strong>{{ duration(view.experiment.elapsedSeconds) }} / {{ duration(view.experiment.remainingSeconds) }}</strong></div>
        <div><span>业务请求</span><strong>{{ view.experiment.businessCount }}</strong></div>
        <div><span>21 / 29 / 错误</span><strong>{{ view.experiment.normalCount }} / {{ view.experiment.degradedCount }} / {{ view.experiment.errorCount }}</strong></div>
        <div><span>切换 / 候选网关</span><strong>{{ Math.max(0, view.experiment.switchCount - (view.experiment.mode === 'cache' ? 0 : 1)) }} / {{ view.experiment.candidateCount }}</strong></div>
      </div>
      <p class="pool-note">
        同步于 {{ timestamp(view.experiment.fetchedAt) }} · 每 5s 自动更新<span v-if="Date.now() / 1000 - view.experiment.fetchedAt > 20"> · 同步延迟，当前为上次记录</span>
      </p>
      <div v-if="view.experiment.mode === 'cache'" class="pool-table-wrap">
        <table>
          <thead>
            <tr>
              <th>阶段</th><th>网关</th><th>地区</th><th v-if="view.experiment.conversationMode === 'append'">
                历史消息
              </th><th>输入 tokens</th><th>缓存 tokens</th><th>命中比例</th><th>输出 tokens</th><th>耗时</th><th>头部信号</th><th>返回 state</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="sample in view.cacheSamples" :key="sample.phase">
              <td>{{ sample.phase }}</td><td>{{ sample.gateway || '—' }}</td><td>{{ sample.datacenterCountry || '—' }}</td><td v-if="view.experiment.conversationMode === 'append'">
                {{ sample.messageCount ?? '—' }}
              </td><td>{{ sample.inputTokens ?? '—' }}</td><td>{{ sample.cachedTokens ?? '—' }}</td><td>{{ sample.inputTokens && sample.cachedTokens != null ? `${(sample.cachedTokens / sample.inputTokens * 100).toFixed(1)}%` : '—' }}</td><td>{{ sample.outputTokens ?? '—' }}</td><td>{{ sample.durationMs != null ? `${(sample.durationMs / 1000).toFixed(2)}s` : '—' }}</td><td>{{ sample.classification || sample.result }}</td><td>{{ sample.stateReturned == null ? '—' : sample.stateReturned ? '是' : '否' }}</td>
            </tr>
          </tbody>
        </table>
        <p class="pool-note">
          仅展示真实 usage；探测请求头后取消，不计入缓存样本。{{ view.experiment.conversationMode === 'append' ? '初始背景' : '完整输入' }} SHA-256：{{ view.experiment.requestSha256 }}
        </p>
      </div>
    </div>
    <details v-if="draft" class="pool-card" :open="!view?.experiment">
      <summary>运行设置</summary>
      <fieldset class="pool-config" :disabled="view?.readOnly">
        <div class="pool-fields" @input="dirty = true" @change="dirty = true">
          <label for="pool-mode">运行模式<select id="pool-mode" v-model="draft.mode"><option value="lazy">懒模式 · 来请求才准备</option><option value="continuous">持续模式 · 无请求也轮换检测</option></select></label>
          <label for="pool-window">准备切换时间（秒）<input id="pool-window" v-model.number="draft.windowSeconds" type="number" min="1" max="3600"></label>
          <label for="pool-interval">候选探测间隔（毫秒）<input id="pool-interval" v-model.number="draft.probeIntervalMs" type="number" min="1" max="1000"></label>
          <label for="pool-initial">初始静置（小时）<input id="pool-initial" v-model.number="initialHours" type="number" min="0.01" max="24" step="0.25"></label>
          <label for="pool-maximum">最大静置（小时）<input id="pool-maximum" v-model.number="maximumHours" type="number" min="0.01" max="24" step="0.25"></label>
          <label for="pool-url">池服务地址<input id="pool-url" v-model="draft.poolUrl" type="url"></label>
          <label for="pool-token">池服务凭证<input id="pool-token" v-model="token" type="password" autocomplete="new-password" :placeholder="draft.poolTokenConfigured ? '已设置，留空保留' : '填写池服务凭证'"></label>
          <fieldset><legend>使用此策略的分组</legend><label v-for="group in groups" :key="group.id" :for="`pool-group-${group.id}`" class="pool-check"><input :id="`pool-group-${group.id}`" v-model="draft.groupIds" type="checkbox" :value="group.id">{{ group.name }}</label><span v-if="!groups.length">暂无分组</span></fieldset>
          <label for="pool-gateways">候选网关<textarea id="pool-gateways" v-model="gatewayText" rows="3" placeholder="unified-142&#10;unified-143" /></label>
        </div>
        <p class="pool-note">
          静置失败翻倍，最高 24h；连续两轮同档恢复才学习起点。切换后保留在途请求，冷却与统计持久保存。
        </p>
        <div v-if="eligible.length" class="pool-mappings">
          <label v-for="account in eligible" :key="account.id" :for="`pool-account-${account.id}`">{{ account.name || account.email }}<input :id="`pool-account-${account.id}`" v-model="draft.poolAccountIds[account.id]" placeholder="池服务的 account 标识" @input="dirty = true"></label>
        </div>
      </fieldset>
    </details>
    <div class="pool-card pool-stats">
      <div class="pool-toolbar">
        <nav aria-label="网关池统计">
          <button v-for="[value, label] in [['pairs', '组合状态'], ['observations', '静置与检测'], ['switches', '切换记录']]" :key="value" :class="{ selected: tab === value }" @click="tab = value">
            {{ label }}
          </button>
        </nav>
        <select v-model="accountFilter" aria-label="筛选账号" @change="refresh">
          <option value="">
            全部账号
          </option><option v-for="account in accounts" :key="account.id" :value="account.id">
            {{ account.name || account.email }}
          </option>
        </select>
        <select v-model="gatewayFilter" aria-label="筛选网关" @change="refresh">
          <option value="">
            全部网关
          </option><option v-for="gateway in draft?.gateways" :key="gateway">
            {{ gateway }}
          </option>
        </select>
      </div>
      <div class="pool-table-wrap">
        <table v-if="tab === 'pairs'">
          <thead><tr><th>账号</th><th>网关</th><th>状态</th><th>窗口剩余</th><th>在途</th><th>静置档位</th><th>学习起点</th><th>冷却截止</th></tr></thead><tbody>
            <tr v-for="pair in view?.pairs" :key="`${pair.accountId}:${pair.gateway}`">
              <td>{{ names.get(pair.accountId) || pair.accountId }}</td><td>{{ pair.gateway }}</td><td>{{ states[pair.state] || pair.state }}</td><td>{{ pair.state === 'ready' && pair.firstContactAt ? duration(Math.max(0, pair.firstContactAt + (draft?.windowSeconds || 180) - Date.now() / 1000)) : '—' }}</td><td>{{ pair.inFlight }}</td><td>{{ duration(pair.restSeconds) }}</td><td>{{ duration(pair.learnedRestSeconds) }}</td><td>{{ timestamp(pair.restUntil) }}</td>
            </tr>
          </tbody>
        </table>
        <table v-else-if="tab === 'observations'">
          <thead><tr><th>时间</th><th>账号 / 网关</th><th>来源</th><th>HTTP</th><th>信号</th><th>返回 state</th><th>计划静置</th><th>实际静置</th><th>耗时</th></tr></thead><tbody>
            <tr v-for="sample in view?.observations" :key="sample.id">
              <td>{{ timestamp(sample.at) }}</td><td>{{ names.get(sample.account) || sample.account }} / {{ sample.gateway }}</td><td>{{ source(sample) }}</td><td>{{ sample.status ?? '—' }}</td><td>{{ sample.classification ?? '未判定' }}</td><td>{{ sample.stateReturned ? '是' : '否' }}</td><td>{{ duration(sample.plannedRestSeconds) }}</td><td>{{ duration(sample.observedIdleSeconds) }}</td><td>{{ sample.durationMs == null ? '—' : `${sample.durationMs}ms` }}</td>
            </tr>
          </tbody>
        </table>
        <table v-else>
          <thead><tr><th>切换时间</th><th>账号</th><th>原网关</th><th>新网关</th><th>旧组合使用时长</th><th>准备耗时</th></tr></thead><tbody>
            <tr v-for="(item, index) in view?.switches" :key="index">
              <td>{{ timestamp(item.at) }}</td><td>{{ names.get(item.accountId) || item.accountId }}</td><td>{{ item.fromGateway || '首次准备' }}</td><td>{{ item.toGateway }}</td><td>{{ duration(item.previousWindowSeconds) }}</td><td>{{ duration(item.preparationSeconds) }}</td>
            </tr>
          </tbody>
        </table>
        <p v-if="view && !view[tab as 'pairs' | 'observations' | 'switches'].length" class="pool-empty">
          暂无记录。启用分组并完成检测后，这里显示实际数据。
        </p>
      </div>
    </div>
  </section>
</template>

<style scoped>
.pool-config {
  border: 0;
  padding: 0;
  margin: 0;
  min-width: 0;
}
.pool-experiment-heading {
  display: flex;
  justify-content: space-between;
  gap: 12px;
}
.pool-experiment-metrics {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
  gap: 16px;
  margin: 16px 0;
}
.pool-experiment-metrics div {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.pool-experiment-metrics span {
  color: var(--cp-color-text-secondary);
  font-size: 13px;
}
.pool-experiment-metrics strong {
  font-size: 19px;
  font-variant-numeric: tabular-nums;
}
.pool-page {
  display: flex;
  flex-direction: column;
  gap: 20px;
  padding: 24px;
  min-height: 0;
  color: var(--cp-color-text);
  font: inherit;
}
.pool-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}
.pool-header h1 {
  font-size: 24px;
  font-weight: 600;
  margin: 0;
}
.pool-header p,
.pool-note {
  color: var(--cp-color-text-secondary);
  font-size: 13px;
  margin: 8px 0;
}
.pool-card {
  border: 1px solid var(--cp-color-border);
  border-radius: 12px;
  background: var(--cp-color-bg-container);
  padding: 20px;
}
.pool-card summary {
  font-weight: 600;
  cursor: pointer;
}
.pool-fields {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 16px;
  margin-top: 18px;
}
.pool-fields label,
.pool-mappings label {
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 13px;
}
.pool-page input,
.pool-page select,
.pool-page textarea {
  font: inherit;
  color: inherit;
  background: var(--cp-color-bg-layout);
  border: 1px solid var(--cp-color-border);
  border-radius: 8px;
  padding: 9px 12px;
  min-width: 0;
}
.pool-page input[type='checkbox'] {
  width: auto;
}
.pool-fields fieldset {
  border: 1px solid var(--cp-color-border);
  border-radius: 8px;
  max-height: 150px;
  overflow: auto;
}
.pool-fields .pool-check {
  flex-direction: row;
  align-items: center;
  gap: 8px;
  margin: 6px 0;
}
.pool-mappings {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 12px;
  margin-top: 16px;
}
.pool-toolbar {
  display: flex;
  gap: 12px;
  align-items: center;
  flex-wrap: wrap;
  margin-bottom: 16px;
}
.pool-toolbar nav {
  display: flex;
  gap: 4px;
  margin-right: auto;
}
.pool-toolbar button {
  font: inherit;
  cursor: pointer;
  background: transparent;
  border: 0;
  border-radius: 8px;
  padding: 9px 12px;
  color: var(--cp-color-text-secondary);
}
.pool-toolbar button.selected {
  background: var(--cp-color-bg-elevated);
  color: var(--cp-color-text);
}
.pool-table-wrap {
  overflow: auto;
}
.pool-page table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
  white-space: nowrap;
}
.pool-page th,
.pool-page td {
  text-align: left;
  padding: 12px;
  border-bottom: 1px solid var(--cp-color-border);
}
.pool-page th {
  font-weight: 500;
  color: var(--cp-color-text-secondary);
}
.pool-error {
  color: var(--cp-color-error-text);
}
.pool-empty {
  text-align: center;
  padding: 32px;
  color: var(--cp-color-text-secondary);
}
</style>

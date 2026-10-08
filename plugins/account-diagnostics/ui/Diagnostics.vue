<script setup lang="ts">
import { onMounted, onScopeDispose, ref, computed } from 'vue'
import { Activity, FlaskConical, Play, Pencil, Plus, SlidersHorizontal, Zap } from '@lucide/vue'
import { call, context } from '@sdk/ui'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'

interface Template { id: string, name: string, prompt: string }
interface Result { status?: number, stateReturned?: boolean, durationMs?: number, classification?: string | null, text?: string, reason?: string, at?: string, bootstrapPerformed?: boolean }
interface Task { id: string, status: string, result?: Result, error?: string }
const ctx = context()
const templates = ref<Template[]>([])
const selected = ref('hi'), model = ref('gpt-6-astra'), effort = ref('low')
const models = ref(['gpt-6-astra', 'gpt-6.1-sol', 'gpt-6-sol'])
const template = computed(() => templates.value.find(item => item.id === selected.value))
const task = ref<Task>(), error = ref(''), requestText = ref('')
const editOpen = ref(false), draft = ref<Template>({ id: '', name: '', prompt: '' })
const history = ref<{ id: string, result?: Result, status: string }[]>([])
let timer: ReturnType<typeof setTimeout> | undefined, disposed = false
const quickSupported = computed(() => !ctx.provider || ctx.provider === 'openai')
const submitting = ref(false), saving = ref(false)
const running = computed(() => submitting.value || task.value?.status === 'running')
async function loadHistory() { history.value = await call('diagnostics.history', { accountId: ctx.accountId }) }
async function poll() {
  if (!task.value || disposed) return
  try {
    task.value = await call<Task>('tasks.status', { id: task.value.id })
    if (task.value.status === 'running') timer = setTimeout(() => { void poll() }, 750)
    else await loadHistory()
  }
  catch (cause) { error.value = String(cause) }
}
async function start(mode: 'text' | 'quick') {
  if (running.value) return
  submitting.value = true
  clearTimeout(timer)
  error.value = ''
  requestText.value = mode === 'quick' ? 'hi' : template.value?.prompt || 'hi'
  try {
    task.value = await call<Task>('tasks.start', { accountId: ctx.accountId, mode, model: model.value, effort: effort.value, prompt: requestText.value })
    await poll()
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '测试失败' }
  finally { submitting.value = false }
}
function edit(add = false) {
  if (!add && !template.value) return
  draft.value = add ? { id: crypto.randomUUID(), name: '自定义测试', prompt: '' } : { ...template.value! }
  editOpen.value = true
}
async function save() {
  if (saving.value) return
  saving.value = true
  try {
    const items = templates.value.map(item => item.id === draft.value.id ? { ...draft.value } : item)
    if (!items.some(item => item.id === draft.value.id)) items.push({ ...draft.value })
    templates.value = await call<Template[]>('templates.write', { items })
    selected.value = draft.value.id
    editOpen.value = false
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '保存失败' }
  finally { saving.value = false }
}
async function cancel() {
  if (!task.value) return
  try {
    await call('tasks.cancel', { id: task.value.id })
    clearTimeout(timer)
    await poll()
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '取消失败' }
}
onMounted(async () => {
  try {
    templates.value = await call<Template[]>('templates.read', {})
    if (!templates.value.some(item => item.id === selected.value)) selected.value = templates.value[0]?.id || ''
    await loadHistory()
    const available = await call<{ models: { id: string }[] }>('accounts.get.admin.accounts.models', { accountId: ctx.accountId })
    if (available.models?.length) models.value = [...new Set([...models.value, ...available.models.map(item => item.id)])]
  }
  catch (cause) { error.value = cause instanceof Error ? cause.message : '加载失败' }
})
onScopeDispose(() => { disposed = true; clearTimeout(timer) })
</script>
<template>
  <div class="diagnostics text-cp text-cp-text">
    <p class="diagnostics-account text-cp-sm text-cp-text-secondary">{{ ctx.accountName || '当前账号' }}</p>
    <div class="diagnostics-body">
      <section class="diagnostics-results rounded-cp border border-cp-border-secondary bg-cp-bg-layout">
        <h3 class="diagnostics-heading"><Activity :size="16" />测试结果</h3>
        <BaseScrollbar class="min-h-0 flex-1">
          <div class="diagnostics-result-body" aria-live="polite">
            <div v-if="!task" class="diagnostics-empty text-cp-text-secondary">
              <FlaskConical :size="28" class="text-cp-primary-text" />
              <strong class="text-cp-lg text-cp-text">准备就绪</strong>
              <p>选择测试模板，或直接开始快速降智检测。</p>
            </div>
            <p v-if="running" class="text-cp-primary-text">正在执行，检测流程会自动继续。</p>
            <template v-if="task?.result">
              <div class="rounded-cp bg-cp-bg-container p-3">
                <p class="font-emphasis" :class="task.result.classification === '29' ? 'text-cp-warning-text' : 'text-cp-text'">
                  {{ task.result.classification === '29' ? '29 类 · 降智信号' : task.result.classification === '21' ? '21 类 · 正常信号' : task.result.text ? '测试完成' : '检测失败，不作判定' }}
                </p>
                <div class="mt-3 grid grid-cols-3 gap-3 text-cp-sm">
                  <div><span class="text-cp-text-secondary">HTTP 状态</span><p class="mt-1 font-emphasis">{{ task.result.status ?? '—' }}</p></div>
                  <div><span class="text-cp-text-secondary">耗时</span><p class="mt-1 font-emphasis">{{ task.result.durationMs ?? '—' }} ms</p></div>
                  <div><span class="text-cp-text-secondary">新 state</span><p class="mt-1 font-emphasis">{{ task.result.stateReturned == null ? '—' : task.result.stateReturned ? '是' : '否' }}</p></div>
                </div>
              </div>
              <p v-if="task.result.bootstrapPerformed" class="mt-3 text-cp-xs text-cp-text-secondary">已自动获取配套材料，判定属于随后重放的请求。</p>
              <p class="mt-4 whitespace-pre-wrap break-words leading-relaxed">{{ task.result.text || task.result.reason }}</p>
            </template>
            <p v-if="task?.error || error" role="alert" class="mt-3 text-cp-error-text">{{ task?.error || error }}</p>
            <details v-if="requestText" class="mt-5 text-cp-sm text-cp-text-secondary"><summary>本次测试内容</summary><p class="mt-2 whitespace-pre-wrap break-words leading-relaxed">{{ requestText }}</p></details>
            <details v-if="history.length" class="mt-5 text-cp-sm text-cp-text-secondary">
              <summary>最近检测（{{ history.length }}）</summary>
              <div v-for="item in [...history].reverse()" :key="item.id" class="mt-3 flex flex-wrap gap-x-3 gap-y-1 border-b border-cp-border-secondary pb-2">
                <span>{{ item.result?.at ? new Date(item.result.at).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' }) : '任务未完成' }}</span>
                <span>HTTP {{ item.result?.status ?? '—' }}</span><span>{{ item.result?.classification ? `${item.result.classification} 类` : item.status }}</span><span>{{ item.result?.durationMs ?? '—' }} ms</span>
              </div>
            </details>
          </div>
        </BaseScrollbar>
      </section>
      <BaseScrollbar class="diagnostics-config">
        <div class="space-y-4 pr-2">
          <h3 class="diagnostics-heading"><SlidersHorizontal :size="16" />测试设置</h3>
          <div>
            <div class="mb-2 flex items-center justify-between gap-2"><label class="text-cp-sm font-emphasis">测试模板</label><div class="flex gap-1"><BaseIconButton label="编辑模板" size="sm" :disabled="running || !template" @click="edit()"><Pencil :size="15" /></BaseIconButton><BaseIconButton label="添加模板" size="sm" :disabled="running" @click="edit(true)"><Plus :size="15" /></BaseIconButton></div></div>
            <BaseSelect v-model="selected" aria-label="测试模板" :disabled="running" :options="templates.map(item => ({ label: item.name, value: item.id }))" />
          </div>
          <div><label class="mb-2 block text-cp-sm font-emphasis">模型</label><BaseSelect v-model="model" aria-label="模型" :disabled="running" :options="models.map(value => ({ label: value, value }))" /></div>
          <div><label class="mb-2 block text-cp-sm font-emphasis">推理强度</label><BaseSelect v-model="effort" aria-label="推理强度" :disabled="running" :options="['low', 'medium', 'high', 'xhigh'].map(value => ({ label: value, value }))" /></div>
          <details v-if="quickSupported" class="rounded-cp bg-cp-bg-layout p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
            <summary>快速检测规则</summary>
            <p class="mt-2">固定发送 hi，自动取得配套材料并重放一次。读取上游响应头后关闭连接，不切账号、不换出口。</p>
            <p class="mt-2">200 + 新 state：29 类；200 + 无新 state：21 类。401、429、超时等仅记失败。21／29 是实验信号，模型名称不作为判定依据。</p>
          </details>
        </div>
      </BaseScrollbar>
    </div>
    <footer class="diagnostics-actions">
      <BaseButton v-if="running" variant="ghost" :disabled="submitting" @click="cancel">取消等待</BaseButton>
      <BaseButton v-if="quickSupported" variant="soft" :disabled="running" @click="start('quick')"><Zap :size="16" />快速降智检测</BaseButton>
      <BaseButton variant="primary" :disabled="running || !template" @click="start('text')"><Play :size="16" />开始测试</BaseButton>
    </footer>
    <BaseModal v-model="editOpen" title="编辑测试模板" size="md-wide">
      <div class="space-y-4"><div><label class="mb-2 block text-cp-sm">模板名称</label><BaseInput v-model="draft.name" aria-label="模板名称" /></div><div><label class="mb-2 block text-cp-sm">测试内容</label><BaseTextarea v-model="draft.prompt" aria-label="测试内容" :rows="8" /></div></div>
      <template #footer><BaseButton @click="editOpen = false">取消</BaseButton><BaseButton variant="primary" :loading="saving" :disabled="!draft.name.trim() || !draft.prompt.trim()" @click="save">保存模板</BaseButton></template>
    </BaseModal>
  </div>
</template>

<style scoped>
.diagnostics { display: flex; flex: 1; flex-direction: column; min-height: 0; height: 100%; gap: 1rem; }
.diagnostics-account { flex-shrink: 0; overflow-wrap: anywhere; }
.diagnostics-body { display: grid; grid-template-columns: minmax(0, 1fr) 280px; gap: 1.25rem; flex: 1; min-height: 0; }
.diagnostics-results { display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow: hidden; }
.diagnostics-heading { display: flex; align-items: center; gap: .5rem; font-size: var(--cp-font-size); font-weight: 600; flex-shrink: 0; }
.diagnostics-results > .diagnostics-heading { padding: 1rem 1rem 0; }
.diagnostics-result-body { padding: 1rem; }
.diagnostics-empty { display: flex; flex-direction: column; align-items: center; text-align: center; gap: .75rem; padding: 2.5rem .5rem; }
.diagnostics-config { min-height: 0; min-width: 0; }
.diagnostics-actions { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: .5rem; flex-shrink: 0; padding-top: 1rem; border-top: 1px solid var(--cp-border-secondary); }
@media (max-width: 640px) {
  .diagnostics-body { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(100px, 1fr) minmax(100px, 1fr); gap: .75rem; }
  .diagnostics { gap: .75rem; }
  .diagnostics-actions { padding-top: .75rem; }
}
</style>

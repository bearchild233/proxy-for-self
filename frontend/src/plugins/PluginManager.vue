<script setup lang="ts">
import type { PlatformPlugin } from './platform'
import { Box, ChevronDown, GripVertical, LockKeyhole, Puzzle, RefreshCw } from '@lucide/vue'
import { useDocumentVisibility } from '@vueuse/core'
import { useSortable } from '@vueuse/integrations/useSortable'
import { computed, onMounted, ref, shallowRef, useTemplateRef } from 'vue'
import request from '@/api/request'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { formatDateTime } from '@/utils/date'
import { openPlugin, refreshPlatform } from './platform'

const busy = ref<string>()
const error = ref('')
const rollbackTarget = shallowRef<PlatformPlugin>()
async function confirmRollback() {
  if (rollbackTarget.value && await action(rollbackTarget.value, 'rollback'))
    rollbackTarget.value = undefined
}
const packageInput = ref<HTMLInputElement>()
async function upload(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0]
  if (!file)
    return
  if (file.size > 16 * 1024 * 1024) {
    error.value = '插件包不能超过 16 MiB'
    return
  }
  busy.value = 'upload'
  error.value = ''
  try {
    const buffer = await file.arrayBuffer()
    const bytes = new Uint8Array(buffer)
    const sha256 = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', buffer))).map(n => n.toString(16).padStart(2, '0')).join('')
    let text = ''
    for (let i = 0; i < bytes.length; i += 8192)
      text += String.fromCharCode(...bytes.subarray(i, i + 8192))
    await request({ url: '/api/plugin-platform/upload', method: 'POST', data: { package: btoa(text), sha256 } })
    await load()
    await refreshPlatform()
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : '安装失败'
  }
  finally {
    busy.value = undefined
    if (packageInput.value)
      packageInput.value.value = ''
  }
}
const rows = shallowRef<PlatformPlugin[]>([])
const category = ref('all')
const expanded = ref<string>()
const categories = [{ id: 'all', name: '全部' }, { id: 'account-tools', name: '账号工具' }, { id: 'clients', name: '客户端接入' }, { id: 'extensions', name: '功能扩展' }, { id: 'operations', name: '运维工具' }, { id: 'basic', name: '基础' }]
const visible = computed(() => rows.value.filter(p => category.value === 'all' || p.category === category.value))
const container = useTemplateRef<HTMLElement>('container')
let savingOrder = false
let loadGeneration = 0
async function load(silent = false) {
  const generation = ++loadGeneration
  const result = await request<PlatformPlugin[]>({ silent, url: '/api/plugin-platform/manage',
  })
  if (generation === loadGeneration && (!silent || (!busy.value && !savingOrder)))
    rows.value = result
}
async function setLazyLoad(plugin: PlatformPlugin, lazyLoad: boolean) {
  if (busy.value)
    return
  busy.value = plugin.id
  error.value = ''
  try {
    await request({ url: '/api/plugin-platform/loading', method: 'POST', data: { id: plugin.id, lazyLoad } })
    await load()
    await refreshPlatform()
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : '加载设置保存失败'
  }
  finally { busy.value = undefined }
}
async function action(plugin: PlatformPlugin, operation: string) {
  if (busy.value)
    return
  busy.value = plugin.id
  error.value = ''
  try {
    await request({ url: '/api/plugin-platform/action', method: 'POST', data: { id: plugin.id, action: operation, ...(operation === 'rollback' ? { expectedCurrent: plugin.contentVersion, expectedPrevious: plugin.previousRelease?.contentVersion } : {}) } })
    await load()
    await refreshPlatform()
    return true
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : '操作失败'
  }
  finally {
    busy.value = undefined
  }
}
useSortable(container, shallowRef([]), {
  handle: '[data-plugin-handle]',
  draggable: '[data-plugin-id]',
  animation: 170,
  forceFallback: true,
  fallbackTolerance: 3,
  onUpdate: () => { },
  onEnd: async (event) => {
    event.item.remove()
    event.from.insertBefore(event.item, event.from.children[event.oldIndex ?? 0] ?? null)
    const from = event.oldDraggableIndex
    const to = event.newDraggableIndex
    if (savingOrder || from === undefined || to === undefined || from === to)
      return
    const filtered = [...visible.value]
    if (filtered[from]?.required !== filtered[to]?.required)
      return
    const before = rows.value
    filtered.splice(to, 0, filtered.splice(from, 1)[0]!)
    let cursor = 0
    const ids = new Set(filtered.map(p => p.id))
    rows.value = rows.value.map(p => ids.has(p.id) ? filtered[cursor++]! : p)
    savingOrder = true
    try {
      await request({ url: '/api/plugin-platform/order', method: 'POST', data: { ids: rows.value.map(p => p.id) }, silent: true })
    }
    catch {
      rows.value = before
      error.value = '顺序保存失败，请重试'
    }
    finally {
      savingOrder = false
    }
  },
})
useVisiblePolling(async () => {
  if (busy.value || savingOrder)
    return
  await load(true)
  await refreshPlatform()
}, computed(() => rows.value.some(p => p.operation?.status === 'running') ? 1500 : 30_000), useDocumentVisibility())
onMounted(() => void load())
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="插件管理" description="按功能独立更新；基础模块始终启用" />
    <div class="my-5 flex flex-wrap items-center gap-2">
      <button v-for="item in categories" :key="item.id" type="button" class="rounded-cp px-4 py-2 text-cp-sm" :class="category === item.id ? 'bg-cp-primary text-white' : 'bg-cp-bg-container text-cp-text-secondary'" @click="category = item.id">
        {{ item.name }}
      </button>
      <input ref="packageInput" type="file" aria-label="安装受信任的插件包" accept=".tar.gz,.tgz" class="hidden" @change="upload"><BaseButton class="ml-auto" :disabled="!!busy" @click="packageInput?.click()">
        安装插件包
      </BaseButton><BaseButton @click="load()">
        <RefreshCw class="size-4" />刷新
      </BaseButton>
    </div>
    <p v-if="error" role="alert" class="text-cp-error-text">
      {{ error }}
    </p>
    <div ref="container" class="grid gap-3">
      <article v-for="plugin in visible" :key="plugin.id" :data-plugin-id="plugin.id" class="rounded-cp-card bg-cp-bg-container p-4">
        <div class="flex items-center gap-3">
          <button type="button" data-plugin-handle class="cursor-grab text-cp-text-tertiary" :aria-label="`拖动 ${plugin.name}`">
            <GripVertical class="size-4" />
          </button>
          <component :is="plugin.required ? Box : Puzzle" class="size-9 rounded-cp bg-cp-primary-container p-2 text-cp-primary-text" />
          <button type="button" class="min-w-0 flex-1 text-left" :aria-expanded="expanded === plugin.id" @click="expanded = expanded === plugin.id ? undefined : plugin.id">
            <span class="flex items-center gap-2 font-emphasis">{{ plugin.name }}<LockKeyhole v-if="plugin.required" class="size-3.5 text-cp-text-tertiary" /><ChevronDown class="size-4 transition-transform" :class="expanded === plugin.id ? 'rotate-180' : ''" /></span>
            <span class="mt-1 block text-cp-xs text-cp-text-secondary">{{ plugin.required ? '基础模块 · 始终启用' : plugin.installed ? plugin.enabled ? '已启用' : '已停用' : '未安装' }} · {{ plugin.version ? `v${plugin.version}` : '可安装' }}</span>
          </button>
          <BaseButton v-if="!plugin.installed" :disabled="!!busy || plugin.operation?.status === 'running'" @click="plugin.bundled ? action(plugin, 'install') : packageInput?.click()">
            安装
          </BaseButton>
          <template v-else>
            <BaseButton :disabled="!!busy || plugin.operation?.status === 'running'" @click="plugin.id === 'excel-bridge' ? action(plugin, 'update') : packageInput?.click()">
              {{ plugin.id === 'excel-bridge' ? '更新 Worker' : '更新' }}
            </BaseButton>
            <BaseButton v-if="!plugin.required" :disabled="!!busy || plugin.operation?.status === 'running'" @click="action(plugin, plugin.enabled ? 'disable' : 'enable')">
              {{ plugin.enabled ? '停用' : '启用' }}
            </BaseButton>
          </template>
        </div>
        <div v-if="expanded === plugin.id" class="ml-7 mt-4 grid gap-3 border-t border-cp-border-secondary pt-4 text-cp-sm text-cp-text-secondary">
          <p>{{ plugin.description }}</p>
          <div class="flex flex-wrap items-center gap-x-4 gap-y-1">
            <BaseCheckbox :model-value="!!plugin.lazyLoad" :disabled="!!busy" label="懒加载" @update:model-value="value => setLazyLoad(plugin, value)" />
            <span class="text-cp-xs text-cp-text-tertiary">开启后首次使用才加载代码；默认提前加载代码和样式</span>
          </div>
          <p v-if="plugin.id === 'excel-bridge'">
            界面包可直接安装；Worker 更新使用安装器中已校验的 Linux 运行包。更新等待现有工作簿请求完成，失败恢复原版本。
          </p>
          <p v-if="plugin.id === 'backup'">
            计划准入与清理规则随插件包更新。停用后不领取新任务，已开始的备份继续完成；备份数据与存储配置保留。
          </p>
          <p v-if="plugin.id === 'account-diagnostics'">
            普通测试保留完整回答；快速检测只读取响应头。记录账号、时间、状态、耗时与实验分类，不展示 Cookie、票据或密钥。
          </p>
          <p v-if="plugin.id === 'client-access'">
            使用当前密钥生成配置，不创建额外账号授权。Key 用户只能使用自己的密钥与模型范围。
          </p>
          <BaseButton v-if="plugin.id === 'excel-bridge' && plugin.enabled" @click="openPlugin(plugin.id)">
            查看运行状态
          </BaseButton><p v-if="plugin.operation" :class="plugin.operation.status === 'failed' ? 'text-cp-error-text' : ''">
            {{ plugin.operation.message }}
          </p>
          <div class="grid gap-2 sm:grid-cols-2">
            <span>更新范围：独立界面包{{ ['excel-bridge', 'account-diagnostics', 'backup', 'pricing'].includes(plugin.id) ? '与后台逻辑' : '' }}</span>
          </div>
          <div v-if="plugin.currentRelease" class="grid gap-3 lg:grid-cols-2">
            <section v-for="release in [{ label: '当前版本', value: plugin.currentRelease }, ...(plugin.canRollback && plugin.previousRelease ? [{ label: '可回退版本', value: plugin.previousRelease }] : [])]" :key="release.label" class="min-w-0 rounded-cp bg-cp-fill-quaternary p-3">
              <h3 class="font-emphasis text-cp-text">
                {{ release.label }} · v{{ release.value.version }}
              </h3>
              <p class="mt-1 text-cp-xs">
                构建 {{ release.value.contentVersion.slice(0, 12) }} · {{ release.value.releasedAt ? formatDateTime(release.value.releasedAt) : '未提供发布时间' }}
              </p>
              <ul v-if="release.value.notes.length" class="mt-2 list-disc space-y-1 pl-4">
                <li v-for="note in release.value.notes" :key="note">
                  {{ note }}
                </li>
              </ul>
              <p v-else class="mt-2">
                此版本未提供更新说明。
              </p>
            </section>
          </div>
          <p>{{ plugin.required ? '提供控制台基础功能，不允许停用或卸载；更新失败保留当前可用版本。' : '停用后移除菜单、页面和新任务入口。宿主中的账号、密钥及历史用量保留。' }}</p>
          <details>
            <summary class="cursor-pointer">
              声明的能力（{{ plugin.capabilities.length }}）
            </summary><p class="mt-2 break-words font-mono text-cp-xs">
              {{ plugin.capabilities.join(' · ') || '无需网关数据权限' }}
            </p>
          </details>
          <div class="flex gap-2">
            <BaseButton v-if="plugin.canRollback && plugin.previousRelease" :disabled="!!busy || plugin.operation?.status === 'running'" @click="rollbackTarget = plugin">
              回退到 v{{ plugin.previousRelease.version }}
            </BaseButton><BaseButton v-if="!plugin.required && plugin.installed" variant="destructive" :disabled="!!busy || plugin.operation?.status === 'running'" @click="action(plugin, 'uninstall')">
              卸载
            </BaseButton>
          </div>
        </div>
      </article>
    </div>
    <BaseConfirmModal :model-value="!!rollbackTarget" title="确认回退插件" confirm-text="确认回退" :loading="!!busy" @update:model-value="value => { if (!value) rollbackTarget = undefined }" @confirm="confirmRollback">
      <div v-if="rollbackTarget?.previousRelease" class="space-y-3 text-cp-sm font-normal">
        <p v-if="error" role="alert" class="text-cp-error-text">
          {{ error }}
        </p>
        <p class="font-emphasis">
          {{ rollbackTarget.name }}
        </p>
        <p>当前：v{{ rollbackTarget.version }} · {{ rollbackTarget.contentVersion?.slice(0, 12) }}</p>
        <p>回退到：v{{ rollbackTarget.previousRelease.version }} · {{ rollbackTarget.previousRelease.contentVersion.slice(0, 12) }}</p>
        <p>目标发布时间：{{ rollbackTarget.previousRelease.releasedAt ? formatDateTime(rollbackTarget.previousRelease.releasedAt) : '未提供' }}</p>
        <ul v-if="rollbackTarget.previousRelease.notes.length" class="list-disc space-y-1 pl-4">
          <li v-for="note in rollbackTarget.previousRelease.notes" :key="note">
            {{ note }}
          </li>
        </ul>
        <p v-else>
          目标版本未提供更新说明，无法从说明判断其功能差异。
        </p>
        <p>回退此插件包后，重新打开对应页面生效。账号、密钥和历史记录不会回退，网关无需重启；已打开页面和进行中的任务继续使用原版本。</p>
      </div>
    </BaseConfirmModal>
  </div>
</template>

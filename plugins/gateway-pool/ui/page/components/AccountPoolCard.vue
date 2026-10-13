<script setup lang="ts">
import type { CatalogEntry, CoolingVisit, Pair } from '../model'
import { ChevronDown, Radio, Ticket } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { historyTime } from '../history'
import { clock, duration, gatewayNumber, stateLabels } from '../model'

const props = defineProps<{
  name: string
  pairs: Pair[]
  windowSeconds: number
  probeModel: string
  mode: 'lazy' | 'continuous'
  now: number
  catalog?: { at: number, gateways: CatalogEntry[] }
}>()

const expanded = shallowRef(false)
const selectedGateway = shallowRef('')
const recordedGateways = computed(() => props.pairs.filter(pair => pair.coolingSummary?.visits.length).length)
const current = computed(() => props.pairs.find(pair => pair.state === 'ready'))
const preparing = computed(() => props.pairs.find(pair => pair.state === 'preparing'))
const cooling = computed(() => props.pairs.filter(pair => pair.state === 'cooling' && (pair.restUntil ?? 0) > props.now))
const available = computed(() => props.pairs.filter(pair => pair.state === 'unknown' || (pair.state === 'cooling' && (pair.restUntil ?? 0) <= props.now)))
const nextRecovery = computed(() => Math.min(...cooling.value.map(pair => pair.restUntil ?? Infinity)))
const elapsed = computed(() => current.value?.firstContactAt ? props.now - current.value.firstContactAt : 0)
const progress = computed(() => Math.min(1, elapsed.value / Math.max(1, props.windowSeconds)))
const cookieEnding = computed(() => current.value?.cookieExpiresAt != null && current.value.cookieExpiresAt - props.now <= 60)
const overdue = computed(() => elapsed.value >= props.windowSeconds || cookieEnding.value)
const idle = computed(() => current.value?.lastContactAt ? props.now - current.value.lastContactAt : 0)
// 窗口用完后的三种情形：正在准备、懒模式等待下个请求、持续模式等待准备。
const phase = computed(() => {
  if (!overdue.value)
    return { text: '满血窗口', tone: 'text-cp-text-secondary', bar: 'bg-cp-success' }
  const reason = cookieEnding.value && elapsed.value < props.windowSeconds ? 'Cookie 即将到期' : '窗口已用完'
  if (preparing.value)
    return { text: `${reason}，正在准备下一个网关`, tone: 'text-cp-info-text', bar: 'bg-cp-info' }
  if (props.mode === 'lazy')
    return { text: `${reason} · 下个请求到来时切换`, tone: 'text-cp-text-tertiary', bar: 'bg-cp-text-quaternary' }
  return { text: `${reason}，等待准备下一个`, tone: 'text-cp-warning-text', bar: 'bg-cp-warning' }
})
// 当前组合的票：打票模型自带，其余是窗口内按需拿到的。
const tickets = computed(() => current.value ? [props.probeModel, ...(current.value.ticketModels ?? []).filter(model => model !== props.probeModel)] : [])
const entries = computed(() => new Map((props.catalog?.gateways ?? []).map(entry => [entry.gateway, entry])))
// Cookie 剩余以组合记录的到期时间为准（Cookie 自身与池服务有效期取较早者）。
const cookieLeft = computed(() => current.value?.cookieExpiresAt ? current.value.cookieExpiresAt - props.now : null)
// 池服务目录里还没有访问过的网关同样是候选，按“可检测”展示。
const fresh = computed(() => (props.catalog?.gateways ?? [])
  .filter(entry => !props.pairs.some(pair => pair.gateway === entry.gateway))
  .map(entry => ({ accountId: '', gateway: entry.gateway, state: 'unknown', inFlight: 0 }) as Pair))
const order: Record<string, number> = { ready: 0, preparing: 1, draining: 2, cooling: 3, unknown: 4 }
const sorted = computed(() => [...props.pairs, ...fresh.value].sort((a, b) =>
  order[a.state] - order[b.state] || (a.restUntil ?? 0) - (b.restUntil ?? 0) || a.gateway.localeCompare(b.gateway, undefined, { numeric: true })))
const selected = computed(() => sorted.value.find(pair => pair.gateway === selectedGateway.value))

function chipClass(pair: Pair) {
  if (pair.state === 'ready')
    return 'border-cp-success-border bg-cp-success-container text-cp-success-on-container'
  if (pair.state === 'preparing')
    return 'border-cp-info-border bg-cp-info-container text-cp-info-on-container'
  if (pair.state === 'draining')
    return 'border-cp-warning-border bg-cp-warning-container text-cp-warning-on-container'
  if (pair.state === 'cooling' && (pair.restUntil ?? 0) > props.now)
    return 'border-cp-border bg-cp-fill-tertiary text-cp-text-tertiary'
  return 'border-dashed border-cp-border text-cp-text-secondary'
}
function chipTitle(pair: Pair) {
  const entry = entries.value.get(pair.gateway)
  const parts = [`${pair.gateway}${entry?.country ? ` · ${entry.country}` : ''} · ${stateLabels[pair.state]}`]
  if (entry)
    parts.push(`池中 Cookie 剩余 ${duration(entry.validForS - (props.now - (props.catalog?.at ?? props.now)))}`)
  else if (props.catalog)
    parts.push('当前不在池服务目录中')
  if (pair.state === 'cooling' && pair.restUntil)
    parts.push(pair.restUntil > props.now ? `还需静置 ${duration(pair.restUntil - props.now)}` : '静置已结束，可重新检测')
  if (pair.restSeconds)
    parts.push(`当前档位 ${duration(pair.restSeconds)}`)
  if (pair.learnedRestSeconds)
    parts.push(`已学习起点 ${duration(pair.learnedRestSeconds)}`)
  if (pair.retiredReason === '29')
    parts.push('上次退役原因：降智信号')
  else if (pair.retiredReason === 'probe_failed')
    parts.push('上次退役原因：检测失败')
  return parts.join('\n')
}
function visitResult(visit: CoolingVisit) {
  if (visit.result === '21')
    return '21 · 已观测恢复'
  if (visit.result === '29')
    return '29 · 未恢复'
  if (visit.result === 'failed')
    return '访问失败'
  return '访问中 · 待结果'
}
function idleSince(pair: Pair) {
  const current = pair.coolingSummary?.current
  return current?.probeAt == null ? current?.startedAt : null
}
</script>

<template>
  <article class="rounded-cp-lg border border-cp-border bg-cp-bg-container">
    <div class="flex items-center">
      <button
        type="button"
        class="flex min-w-0 flex-1 cursor-pointer items-center gap-4 px-4 py-3.5 text-left"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
      >
        <div class="grid min-w-0 flex-1 gap-2 md:grid-cols-[minmax(0,1.1fr)_minmax(0,1.6fr)_minmax(0,1fr)] md:items-center md:gap-6">
          <div class="min-w-0">
            <div class="truncate text-cp font-emphasis text-cp-text" :title="name">
              {{ name }}
            </div>
            <div class="mt-1 flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
              <Radio class="size-3.5" :class="current ? 'text-cp-success' : 'text-cp-text-quaternary'" aria-hidden="true" />
              <template v-if="current">
                正在使用 <span class="font-mono text-cp-text">{{ current.gateway }}</span>
              </template>
              <template v-else-if="preparing">
                正在检测 <span class="font-mono">{{ preparing.gateway }}</span>
              </template>
              <template v-else>
                暂无可用网关
              </template>
            </div>
          </div>

          <div v-if="current" class="min-w-0">
            <div class="flex items-center justify-between text-cp-xs">
              <span :class="phase.tone">{{ phase.text }}</span>
              <span v-if="!overdue" class="font-mono tabular-nums text-cp-text-secondary">{{ clock(elapsed) }} / {{ clock(windowSeconds) }}</span>
              <span v-else-if="!preparing && idle >= 5" class="tabular-nums text-cp-text-tertiary">已空闲 {{ duration(idle) }}</span>
            </div>
            <div class="mt-1.5 h-1.5 overflow-hidden rounded-full bg-cp-fill-tertiary">
              <div
                class="h-full rounded-full transition-[width] duration-1000 ease-linear"
                :class="phase.bar"
                :style="{ width: `${progress * 100}%` }"
              />
            </div>
          </div>
          <div v-else class="text-cp-xs text-cp-text-tertiary">
            {{ cooling.length && !available.length ? `全部静置，最早 ${duration(nextRecovery - now)} 后可重新检测` : mode === 'lazy' ? '有请求时开始找票' : '持续模式 · 按现有策略准备' }}
          </div>

          <div class="flex flex-wrap items-center gap-1.5 md:justify-end">
            <BaseTag v-for="model in tickets" :key="model" size="sm" :type="model === probeModel ? 'primary' : 'info'">
              <span class="flex items-center gap-1 whitespace-nowrap"><Ticket class="size-3" aria-hidden="true" />{{ model }}</span>
            </BaseTag>
            <BaseTag v-if="cookieLeft != null" size="sm" :type="cookieLeft < 120 ? 'warning' : 'neutral'" title="Cookie 到期前 60 秒开始准备下一个组合">
              <span class="whitespace-nowrap">Cookie {{ duration(cookieLeft) }}</span>
            </BaseTag>
            <BaseTag v-if="current?.inFlight" size="sm" type="neutral">
              在途 {{ current.inFlight }}
            </BaseTag>
          </div>
        </div>
        <div class="hidden shrink-0 items-center gap-3 text-cp-xs text-cp-text-secondary lg:flex">
          <span title="可检测 / 静置中">{{ available.length + fresh.length }} / {{ cooling.length }}</span>
          <ChevronDown class="size-4 transition-transform" :class="{ 'rotate-180': expanded }" aria-hidden="true" />
        </div>
      </button>
      <div v-if="$slots.actions" class="shrink-0 pr-4">
        <slot name="actions" />
      </div>
    </div>

    <div v-if="expanded" class="border-t border-cp-border px-4 py-3.5">
      <div class="mb-3 flex flex-wrap items-center justify-between gap-2 text-cp-xs text-cp-text-secondary">
        <span>静置中 {{ cooling.length }} · 可再次访问 {{ available.length + fresh.length }} · 已有间隔记录 {{ recordedGateways }} / {{ sorted.length }} 个网关</span>
        <span>{{ mode === 'lazy' ? '等待请求触发访问' : '持续模式按策略访问' }} · 冷却汇总见轮转统计</span>
      </div>
      <div class="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-cp-xs text-cp-text-secondary">
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-success" />服务中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-info" />检测中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-warning" />排空中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-fill-secondary" />静置中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm border border-dashed border-cp-text-quaternary" />可检测</span>
      </div>
      <div class="flex max-h-40 flex-wrap gap-1.5 overflow-y-auto" aria-label="网关状态，点击查看单个网关记录">
        <button
          v-for="pair in sorted"
          :key="pair.gateway"
          type="button"
          class="inline-flex items-center gap-1.5 rounded-cp border px-2 py-1 text-left transition-shadow hover:shadow-cp-card focus-visible:outline-2 focus-visible:outline-cp-primary"
          :class="[chipClass(pair), { 'ring-1 ring-cp-primary': selectedGateway === pair.gateway }]"
          :title="chipTitle(pair)"
          :aria-expanded="selectedGateway === pair.gateway"
          :aria-label="`${pair.gateway} 冷却与再访问记录`"
          @click="selectedGateway = selectedGateway === pair.gateway ? '' : pair.gateway"
        >
          <span class="font-mono text-cp-xs">{{ gatewayNumber(pair.gateway) }}</span>
          <span v-if="pair.state === 'cooling' && (pair.restUntil ?? 0) > now" class="text-cp-xs tabular-nums">{{ duration((pair.restUntil ?? 0) - now) }}</span>
        </button>
      </div>
      <div v-if="selected" class="mt-3 rounded-cp bg-cp-fill-quaternary p-3">
        <div class="flex flex-wrap items-center justify-between gap-2 text-cp-xs text-cp-text-secondary">
          <span class="font-emphasis text-cp-text">{{ selected.gateway }} · 冷却与实际再访问</span>
          <span>当前档位 {{ duration(selected.restSeconds) }} · 学习起点 {{ selected.learnedRestSeconds ? duration(selected.learnedRestSeconds) : '未形成' }}</span>
        </div>
        <p v-if="selected.state === 'cooling'" class="mt-2 text-cp-xs text-cp-text-secondary">
          本次静置起点 {{ historyTime(idleSince(selected)) }} · 计划可访问 {{ historyTime(selected.restUntil) }}
          <span v-if="(selected.restUntil ?? 0) <= now"> · 已到期，尚未再次访问，恢复状态未知</span>
        </p>
        <p v-if="selected.coolingSummary?.visits[0]" class="mt-2 text-cp-xs text-cp-text-secondary">
          最近实际间隔 {{ duration(selected.coolingSummary.visits[0].idleSeconds) }} · {{ visitResult(selected.coolingSummary.visits[0]) }}
        </p>
        <p v-else class="mt-2 text-cp-xs text-cp-text-secondary">
          尚无再次访问记录
        </p>
      </div>
      <p v-if="!pairs.length" class="text-cp-xs text-cp-text-tertiary">
        这个账号还没有访问过任何网关。
      </p>
    </div>
  </article>
</template>

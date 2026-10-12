<script setup lang="ts">
import type { Pair } from '../model'
import { ChevronDown, Clock, Radio, Ticket } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { clock, duration, gatewayNumber, stateLabels } from '../model'

const props = defineProps<{
  name: string
  pairs: Pair[]
  windowSeconds: number
  probeModel: string
  now: number
}>()

const expanded = shallowRef(false)
const current = computed(() => props.pairs.find(pair => pair.state === 'ready'))
const preparing = computed(() => props.pairs.find(pair => pair.state === 'preparing'))
const cooling = computed(() => props.pairs.filter(pair => pair.state === 'cooling' && (pair.restUntil ?? 0) > props.now))
const available = computed(() => props.pairs.filter(pair => pair.state === 'unknown' || (pair.state === 'cooling' && (pair.restUntil ?? 0) <= props.now)))
const nextRecovery = computed(() => Math.min(...cooling.value.map(pair => pair.restUntil ?? Infinity)))
const elapsed = computed(() => current.value?.firstContactAt ? props.now - current.value.firstContactAt : 0)
const progress = computed(() => Math.min(1, elapsed.value / Math.max(1, props.windowSeconds)))
const overdue = computed(() => elapsed.value >= props.windowSeconds)
// 当前组合的票：打票模型自带，其余是窗口内按需拿到的。
const tickets = computed(() => current.value ? [props.probeModel, ...(current.value.ticketModels ?? []).filter(model => model !== props.probeModel)] : [])
const order: Record<string, number> = { ready: 0, preparing: 1, draining: 2, cooling: 3, unknown: 4 }
const sorted = computed(() => [...props.pairs].sort((a, b) =>
  order[a.state] - order[b.state] || (a.restUntil ?? 0) - (b.restUntil ?? 0) || a.gateway.localeCompare(b.gateway, undefined, { numeric: true })))

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
  const parts = [`${pair.gateway} · ${stateLabels[pair.state]}`]
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
</script>

<template>
  <article class="rounded-cp-lg border border-cp-border bg-cp-bg-container">
    <button
      type="button"
      class="flex w-full cursor-pointer items-center gap-4 px-4 py-3.5 text-left"
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
            <span :class="overdue ? 'text-cp-warning' : 'text-cp-text-secondary'">
              {{ overdue ? (preparing ? '窗口已到，正在准备下一个' : '窗口已到，继续服务至下一个就绪') : '满血窗口' }}
            </span>
            <span class="font-mono tabular-nums text-cp-text-secondary">{{ clock(elapsed) }} / {{ clock(windowSeconds) }}</span>
          </div>
          <div class="mt-1.5 h-1.5 overflow-hidden rounded-full bg-cp-fill-tertiary">
            <div
              class="h-full rounded-full transition-[width] duration-1000 ease-linear"
              :class="overdue ? 'bg-cp-warning' : 'bg-cp-success'"
              :style="{ width: `${progress * 100}%` }"
            />
          </div>
        </div>
        <div v-else class="text-cp-xs text-cp-text-tertiary">
          {{ cooling.length && !available.length ? `全部静置，最早 ${duration(nextRecovery - now)} 后可重新检测` : '有请求时开始找票' }}
        </div>

        <div class="flex flex-wrap items-center gap-1.5 md:justify-end">
          <BaseTag v-for="model in tickets" :key="model" size="sm" :type="model === probeModel ? 'primary' : 'info'">
            <span class="flex items-center gap-1 whitespace-nowrap"><Ticket class="size-3" aria-hidden="true" />{{ model }}</span>
          </BaseTag>
          <BaseTag v-if="current?.inFlight" size="sm" type="neutral">
            在途 {{ current.inFlight }}
          </BaseTag>
        </div>
      </div>
      <div class="hidden shrink-0 items-center gap-3 text-cp-xs text-cp-text-secondary lg:flex">
        <span title="可检测 / 静置中">{{ available.length }} / {{ cooling.length }}</span>
        <ChevronDown class="size-4 transition-transform" :class="{ 'rotate-180': expanded }" aria-hidden="true" />
      </div>
    </button>

    <div v-if="expanded" class="border-t border-cp-border px-4 py-3.5">
      <div class="mb-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-cp-xs text-cp-text-secondary">
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-success" />服务中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-info" />检测中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-warning" />排空中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm bg-cp-fill-secondary" />静置中</span>
        <span class="flex items-center gap-1.5"><i class="size-2.5 rounded-sm border border-dashed border-cp-text-quaternary" />可检测</span>
      </div>
      <div class="flex flex-wrap gap-1.5">
        <div
          v-for="pair in sorted"
          :key="pair.gateway"
          class="flex min-w-16 flex-col items-center rounded-cp border px-2 py-1"
          :class="chipClass(pair)"
          :title="chipTitle(pair)"
        >
          <span class="font-mono text-cp-xs font-emphasis">{{ gatewayNumber(pair.gateway) }}</span>
          <span v-if="pair.state === 'cooling' && (pair.restUntil ?? 0) > now" class="flex items-center gap-0.5 text-[10px] tabular-nums">
            <Clock class="size-2.5" aria-hidden="true" />{{ duration((pair.restUntil ?? 0) - now) }}
          </span>
          <span v-else class="text-[10px]">{{ pair.state === 'cooling' ? '可检测' : stateLabels[pair.state] }}</span>
        </div>
      </div>
      <p v-if="!pairs.length" class="text-cp-xs text-cp-text-tertiary">
        这个账号还没有访问过任何网关。
      </p>
    </div>
  </article>
</template>

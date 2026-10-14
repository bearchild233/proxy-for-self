<script setup lang="ts">
import type { CoolingCycle, CoolingStep, PoolStatistics } from '../history'
import type { Pair } from '../model'
import { call } from '@sdk/ui'
import { computed, shallowRef, watch } from 'vue'
import { failureLabels, historyTime, reasonLabels } from '../history'
import { duration, stateLabels } from '../model'

const props = defineProps<{ accountId: string, pair: Pair, now: number }>()

const cycles = shallowRef<CoolingCycle[]>([])
const loading = shallowRef(false)
const error = shallowRef('')
const rows = computed(() => cycles.value.map(cycle => ({
  id: cycle.id,
  recovered: cycle.recoveredAt != null,
  // 同一周期内按时间从新到旧展示，档位变化与上一轮比较。
  steps: [...cycle.steps].map((step, index, all) => ({ step, previous: all[index - 1] as CoolingStep | undefined })).reverse(),
})))
const reason = computed(() => reasonLabels[props.pair.retiredReason ?? ''] ?? '')
const cooling = computed(() => props.pair.state === 'cooling')
const standbyLeft = computed(() => props.pair.standbyExpiresAt == null ? null : props.pair.standbyExpiresAt - props.now)

let revision = 0
async function load() {
  const current = ++revision
  loading.value = true
  try {
    const response = await call<PoolStatistics>('gateway-pool.history.read', {
      view: 'cooling',
      from: 0,
      to: Date.now() / 1000,
      accountId: props.accountId,
      gateway: props.pair.gateway,
      page: 1,
      pageSize: 5,
    })
    if (current !== revision)
      return
    cycles.value = response.recoveries
    error.value = ''
  }
  catch (cause) {
    if (current === revision)
      error.value = cause instanceof Error ? cause.message : '读取静置历史失败'
  }
  finally {
    if (current === revision)
      loading.value = false
  }
}
// 点开单个网关或它的状态变化时重读；页面轮询不重复请求。
watch(() => [props.accountId, props.pair.gateway, props.pair.state, props.pair.generation], load, { immediate: true })

function resultText(step: CoolingStep) {
  if (step.result === '21')
    return '21 · 恢复'
  if (step.result === '29')
    return '29 · 未恢复'
  if (step.result === 'failed') {
    const kind = step.failure ? failureLabels[step.failure] ?? step.failure : '检测失败'
    return step.status != null ? `失败 ${step.status} · ${kind}` : `失败 · ${kind}`
  }
  return step.probeAt != null ? '复测中' : '待复测'
}
function tierText(step: CoolingStep, previous?: CoolingStep) {
  const tier = step.tierSeconds ?? step.plannedSeconds
  const before = previous?.tierSeconds ?? previous?.plannedSeconds
  if (before == null || before === tier)
    return duration(tier)
  return `${duration(before)} → ${duration(tier)}`
}
</script>

<template>
  <div class="mt-3 rounded-cp bg-cp-fill-quaternary p-3">
    <div class="flex flex-wrap items-center justify-between gap-2 text-cp-xs text-cp-text-secondary">
      <span class="font-emphasis text-cp-text">{{ pair.gateway }} · {{ stateLabels[pair.state] }}</span>
      <span>
        当前档 {{ pair.tierSeconds ?? pair.restSeconds ? duration(pair.tierSeconds ?? pair.restSeconds) : '—' }}
        · 起始档 {{ pair.learnedRestSeconds ? duration(pair.learnedRestSeconds) : pair.startTierSeconds ? duration(pair.startTierSeconds) : '未形成' }}
      </span>
    </div>
    <p v-if="pair.state === 'standby'" class="mt-2 text-cp-xs text-cp-text-secondary">
      拿到时间 {{ historyTime(pair.standbyAt) }}
      <span v-if="standbyLeft != null"> · {{ standbyLeft > 0 ? `${duration(standbyLeft)} 后丢弃` : '已过期，等待丢弃' }}</span>
    </p>
    <p v-else-if="cooling || pair.state === 'draining'" class="mt-2 text-cp-xs text-cp-text-secondary">
      静置原因 {{ reason || '—' }} · 本次静置 {{ duration(pair.restSeconds) }}
      <template v-if="cooling">
        · 下次可复测 {{ historyTime(pair.restUntil) }}
        <span v-if="(pair.restUntil ?? 0) <= now"> · 已到期，尚未复测，恢复状态未知</span>
      </template>
    </p>
    <p v-if="loading && !cycles.length" class="mt-2 text-cp-xs text-cp-text-tertiary">
      正在读取静置历史…
    </p>
    <p v-else-if="error" role="alert" class="mt-2 text-cp-xs text-cp-error-text">
      {{ error }}
    </p>
    <p v-else-if="!rows.length" class="mt-2 text-cp-xs text-cp-text-secondary">
      尚无静置记录
    </p>
    <div v-for="cycle in rows" :key="cycle.id" class="mt-3 overflow-x-auto">
      <div class="mb-1 text-cp-xs text-cp-text-tertiary">
        {{ cycle.recovered ? '已观测恢复的周期' : '未恢复的周期' }}
      </div>
      <table class="w-full min-w-[34rem] text-left text-cp-xs">
        <thead class="text-cp-text-tertiary">
          <tr>
            <th class="py-1 pr-3 font-normal">
              静置起点
            </th>
            <th class="py-1 pr-3 font-normal">
              原因
            </th>
            <th class="py-1 pr-3 font-normal">
              档位
            </th>
            <th class="py-1 pr-3 font-normal">
              计划复测
            </th>
            <th class="py-1 pr-3 font-normal">
              实际复测
            </th>
            <th class="py-1 font-normal">
              结果
            </th>
          </tr>
        </thead>
        <tbody class="text-cp-text-secondary">
          <tr v-for="item in cycle.steps" :key="item.step.scheduledStartAt">
            <td class="py-1 pr-3 tabular-nums">
              {{ historyTime(item.step.startedAt ?? item.step.scheduledStartAt) }}
            </td>
            <td class="py-1 pr-3">
              {{ item.step.reason ? reasonLabels[item.step.reason] ?? item.step.reason : '—' }}
            </td>
            <td class="py-1 pr-3 tabular-nums">
              {{ tierText(item.step, item.previous) }}
            </td>
            <td class="py-1 pr-3 tabular-nums">
              {{ historyTime(item.step.dueAt) }}
            </td>
            <td class="py-1 pr-3 tabular-nums">
              {{ historyTime(item.step.probeAt) }}
            </td>
            <td class="py-1">
              {{ resultText(item.step) }}
              <span v-if="item.step.result === '21' && item.step.learnedRestSeconds" class="text-cp-text-tertiary"> · 起始档 {{ duration(item.step.learnedRestSeconds) }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p v-if="cycles.length" class="mt-2 text-cp-xs text-cp-text-tertiary">
      最近 {{ cycles.length }} 个周期 · 档位列显示相邻两轮的变化；起始档是下次静置的起点
    </p>
  </div>
</template>

<script setup lang="ts">
import type { DirectoryAccount, GroupEvent } from '../model'
import { computed } from 'vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { groupEventLabels } from '../model'

const props = defineProps<{
  events: GroupEvent[]
  accounts: DirectoryAccount[]
  groups: { id: string, name: string }[]
  enabled: boolean
}>()

const accountNames = computed(() => new Map(props.accounts.map(account => [account.id, account.name || account.email || account.id])))
const groupNames = computed(() => new Map(props.groups.map(group => [group.id, group.name])))
const accountName = (id: string) => accountNames.value.get(id) ?? id
const groupName = (id: string) => groupNames.value.get(id) ?? id
const time = (seconds: number) => new Date(seconds * 1000).toLocaleString('zh-CN', { hour12: false })
const tagType = (status: GroupEvent['status']) => status === 'moved' ? 'success' : status === 'failed' ? 'danger' : status === 'pending' ? 'info' : 'neutral'
</script>

<template>
  <div class="grid gap-3">
    <p class="text-cp-xs text-cp-text-secondary">
      {{ enabled ? '自动调整账号分组的每一次决定与结果，保留最近 200 条。' : '动态分组未开启，在设置里开启后这里会记录每次调整。' }}
    </p>
    <div v-if="events.length" class="overflow-x-auto rounded-cp border border-cp-border">
      <table class="w-full min-w-176 text-left text-cp-sm">
        <thead class="bg-cp-fill-quaternary text-cp-xs text-cp-text-secondary">
          <tr>
            <th class="px-3 py-2 font-emphasis">
              时间
            </th>
            <th class="px-3 py-2 font-emphasis">
              账号
            </th>
            <th class="px-3 py-2 font-emphasis">
              原因
            </th>
            <th class="px-3 py-2 font-emphasis">
              分组变化
            </th>
            <th class="px-3 py-2 font-emphasis">
              模型
            </th>
            <th class="px-3 py-2 font-emphasis">
              结果
            </th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="event in events" :key="event.id" class="border-t border-cp-border-secondary">
            <td class="px-3 py-2 font-mono text-cp-xs tabular-nums text-cp-text-secondary">
              {{ time(event.at) }}
            </td>
            <td class="max-w-56 truncate px-3 py-2 text-cp-text" :title="accountName(event.accountId)">
              {{ accountName(event.accountId) }}
            </td>
            <td class="px-3 py-2">
              <BaseTag size="sm" :type="event.direction === 'degraded' ? 'danger' : 'success'">
                {{ event.direction === 'degraded' ? '确认降智' : '持续满血，已恢复' }}
              </BaseTag>
            </td>
            <td class="px-3 py-2 text-cp-text-secondary">
              {{ groupName(event.fromGroup) }} → {{ groupName(event.toGroup) }}
            </td>
            <td class="px-3 py-2 font-mono text-cp-xs text-cp-text-secondary">
              {{ event.model || '—' }}
            </td>
            <td class="px-3 py-2">
              <BaseTag size="sm" :type="tagType(event.status)">
                {{ groupEventLabels[event.status] }}
              </BaseTag>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <BaseEmpty v-else size="sm" title="暂无分组变动" />
  </div>
</template>

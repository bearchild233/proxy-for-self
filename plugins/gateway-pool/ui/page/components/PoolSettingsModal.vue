<script setup lang="ts">
import type { DirectoryAccount, Settings } from '../model'
import { computed, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseNumberInput from '@/components/base/BaseNumberInput.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

const props = defineProps<{
  settings: Settings
  groups: { id: string, name: string }[]
  accounts: DirectoryAccount[]
  saving: boolean
  readOnly?: boolean
}>()
const emit = defineEmits<{ save: [payload: Record<string, unknown>] }>()
const open = defineModel<boolean>({ default: false })

const draft = ref<Settings>(structuredCopy(props.settings))
const initialHours = ref(2)
const maximumHours = ref(24)
const gatewayText = ref('')
const token = ref('')
const showOverrides = ref(false)

function structuredCopy(value: Settings): Settings {
  return JSON.parse(JSON.stringify(value)) as Settings
}
// 每次打开都从当前已保存设置开始，取消不留下草稿。
watch(open, (value) => {
  if (!value)
    return
  draft.value = structuredCopy(props.settings)
  initialHours.value = props.settings.initialRestSeconds / 3600
  maximumHours.value = props.settings.maximumRestSeconds / 3600
  gatewayText.value = props.settings.gateways.join('\n')
  token.value = ''
  showOverrides.value = Object.keys(props.settings.poolAccountIds).length > 0
})

const groupOptions = computed(() => props.groups.map(group => ({ label: group.name, value: group.id })))
// 开启动态分组必须明确两个不同的分组，降智分组须在上面选中的适用分组里。
const dynamicProblem = computed(() => {
  if (!draft.value.dynamicGroups)
    return ''
  if (!draft.value.normalGroupId || !draft.value.degradedGroupId)
    return '请选择正常分组和降智分组'
  if (draft.value.normalGroupId === draft.value.degradedGroupId)
    return '正常分组与降智分组不能相同'
  if (!draft.value.groupIds.includes(draft.value.degradedGroupId))
    return '降智分组须在上面的适用分组中'
  return ''
})
const gateways = computed(() => [...new Set(gatewayText.value.split(/[\s,;]+/).filter(Boolean))])
const invalidGateways = computed(() => gateways.value.filter(item => !/^unified-\d{1,6}$/.test(item)))
const members = computed(() => props.accounts.filter(account => account.groups?.some(group => draft.value.groupIds.includes(group.id))))
const modeOptions = [
  { label: '懒模式', value: 'lazy' },
  { label: '持续模式', value: 'continuous' },
]
const modeHint = computed(() => draft.value.mode === 'lazy'
  ? '没有请求时不找票；窗口到期后由下一个请求触发准备。'
  : '近期有流量的账号在空闲时也按窗口轮换，切换更及时，但会多发检测请求。')

function toggleGroup(id: string, checked: boolean) {
  const ids = new Set(draft.value.groupIds)
  if (checked)
    ids.add(id)
  else
    ids.delete(id)
  draft.value.groupIds = [...ids]
}
function submit() {
  if (invalidGateways.value.length || dynamicProblem.value)
    return
  const overrides = Object.fromEntries(
    Object.entries(draft.value.poolAccountIds).map(([id, value]) => [id, value.trim()]).filter(([, value]) => value),
  )
  emit('save', {
    mode: draft.value.mode,
    windowSeconds: draft.value.windowSeconds,
    probeIntervalMs: draft.value.probeIntervalMs,
    probeModel: draft.value.probeModel.trim(),
    poolDirect: draft.value.poolDirect,
    dynamicGroups: draft.value.dynamicGroups ?? false,
    normalGroupId: draft.value.normalGroupId ?? '',
    degradedGroupId: draft.value.degradedGroupId ?? '',
    groupIds: draft.value.groupIds,
    gateways: gateways.value,
    poolUrl: draft.value.poolUrl.trim(),
    poolAccountIds: overrides,
    initialRestSeconds: initialHours.value * 3600,
    maximumRestSeconds: maximumHours.value * 3600,
    poolToken: token.value || undefined,
  })
}
</script>

<template>
  <BaseModal v-model="open" title="网关池设置" description="作用于选定分组；其他分组保持原有调度。" size="lg">
    <fieldset class="grid gap-6" :disabled="readOnly">
      <section class="grid gap-3">
        <h3 class="text-cp font-emphasis text-cp-text">
          适用分组
        </h3>
        <div v-if="groups.length" class="grid gap-2 sm:grid-cols-2">
          <BaseCheckbox
            v-for="group in groups"
            :key="group.id"
            :label="group.name"
            show-label
            :model-value="draft.groupIds.includes(group.id)"
            @update:model-value="toggleGroup(group.id, $event)"
          />
        </div>
        <p v-else class="text-cp-xs text-cp-text-tertiary">
          暂无分组，请先在分组管理中创建降智专用分组。
        </p>
        <p class="text-cp-xs text-cp-text-tertiary">
          选定分组只接受 HTTP 请求，原生 WS 请求会被拒绝；所有模型共用满血窗口，各模型使用自己的票。
        </p>
      </section>

      <section class="grid gap-3">
        <h3 class="text-cp font-emphasis text-cp-text">
          动态分组
        </h3>
        <div class="flex items-start justify-between gap-4 rounded-cp border border-cp-border px-3 py-2.5">
          <div class="grid gap-0.5">
            <span class="text-cp-sm text-cp-text">按检测结果自动调整账号分组</span>
            <span class="text-cp-xs text-cp-text-tertiary">普通账号在带 state 的 HTTP 请求上被确认降智时，自动从正常分组移到降智分组，并把这次请求交给其他账号；降智账号持续满血恢复后移回。每次调整都记录在“分组变动”里。</span>
          </div>
          <BaseSwitch v-model="draft.dynamicGroups" label="动态分组" />
        </div>
        <div v-if="draft.dynamicGroups" class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="正常分组" description="恢复后账号回到这里">
            <BaseSelect v-model="draft.normalGroupId" aria-label="正常分组" :options="groupOptions" searchable />
          </BaseFormItem>
          <BaseFormItem label="降智分组" description="确认降智的账号移到这里">
            <BaseSelect v-model="draft.degradedGroupId" aria-label="降智分组" :options="groupOptions" searchable />
          </BaseFormItem>
        </div>
        <p v-if="dynamicProblem" role="alert" class="text-cp-xs text-cp-error-text">
          {{ dynamicProblem }}
        </p>
      </section>

      <section class="grid gap-3">
        <h3 class="text-cp font-emphasis text-cp-text">
          运行策略
        </h3>
        <div class="grid gap-2">
          <BaseSegmented v-model="draft.mode" label="运行模式" :options="modeOptions" class="max-w-80" />
          <p class="text-cp-xs text-cp-text-tertiary">
            {{ modeHint }}
          </p>
        </div>
        <div class="grid gap-4 sm:grid-cols-3">
          <BaseFormItem label="打票模型">
            <BaseInput v-model="draft.probeModel" placeholder="gpt-6.1-sol" />
          </BaseFormItem>
          <BaseFormItem label="满血窗口">
            <BaseNumberInput v-model="draft.windowSeconds" label="满血窗口" unit="秒" :min="30" :max="3600" :step="10" />
          </BaseFormItem>
          <BaseFormItem label="候选间隔">
            <BaseNumberInput v-model="draft.probeIntervalMs" label="候选间隔" unit="毫秒" :min="1" :max="1000" :step="10" />
          </BaseFormItem>
        </div>
        <p class="text-cp-xs text-cp-text-tertiary">
          打票模型以 low 发送 hi 检测网关；窗口到期后开始准备下一个组合，新组合验证通过后未发送的请求切换过去，已发送的请求照常完成。
        </p>
      </section>

      <section class="grid gap-3">
        <h3 class="text-cp font-emphasis text-cp-text">
          静置退避
        </h3>
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="初始静置">
            <BaseNumberInput v-model="initialHours" label="初始静置" unit="小时" :min="0.25" :max="24" :step="0.25" />
          </BaseFormItem>
          <BaseFormItem label="最长静置">
            <BaseNumberInput v-model="maximumHours" label="最长静置" unit="小时" :min="0.25" :max="24" :step="1" />
          </BaseFormItem>
        </div>
        <p class="text-cp-xs text-cp-text-tertiary">
          按账号×网关计时，从最后一次访问结束起算。复测仍降智或失败则翻倍；连续两轮在同一档恢复后，以后从该档开始。
        </p>
      </section>

      <section class="grid gap-3">
        <h3 class="text-cp font-emphasis text-cp-text">
          池服务
        </h3>
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="服务地址">
            <BaseInput v-model="draft.poolUrl" placeholder="https://" />
          </BaseFormItem>
          <BaseFormItem label="服务凭证">
            <BaseInput v-model="token" type="password" autocomplete="new-password" :placeholder="settings.poolTokenConfigured ? '已设置，留空保持不变' : '填写池服务 Bearer'" />
          </BaseFormItem>
        </div>
        <div class="flex items-start justify-between gap-4 rounded-cp border border-cp-border px-3 py-2.5">
          <div class="grid gap-0.5">
            <span class="text-cp-sm text-cp-text">池服务直连</span>
            <span class="text-cp-xs text-cp-text-tertiary">取网关目录和 Cookie 时走服务器直连，避免账号代理无法访问池服务；打票与业务请求仍走账号出口。</span>
          </div>
          <BaseSwitch v-model="draft.poolDirect" label="池服务直连" />
        </div>
        <BaseFormItem label="限定网关（可选）" :description="gateways.length ? `只在这 ${gateways.length} 个网关中挑选，且须在池服务目录中` : '留空：跟随池服务当前可配对的网关，池中消失的网关自动不再作为候选'">
          <BaseTextarea v-model="gatewayText" :rows="4" placeholder="unified-142&#10;unified-143" class="font-mono" />
        </BaseFormItem>
        <p v-if="invalidGateways.length" role="alert" class="text-cp-xs text-cp-error-text">
          格式不正确：{{ invalidGateways.slice(0, 5).join('、') }}
        </p>
        <button type="button" class="w-fit text-cp-xs text-cp-primary-text hover:underline" @click="showOverrides = !showOverrides">
          {{ showOverrides ? '收起' : '自定义' }}池成员标识
        </button>
        <div v-if="showOverrides" class="grid gap-2 rounded-cp border border-cp-border p-3">
          <p class="text-cp-xs text-cp-text-tertiary">
            默认按上游账号与用户自动生成，通常无需填写。
          </p>
          <BaseFormItem v-for="account in members" :key="account.id" :label="account.name || account.email || account.id">
            <BaseInput v-model="draft.poolAccountIds[account.id]" size="sm" placeholder="留空自动生成" />
          </BaseFormItem>
          <p v-if="!members.length" class="text-cp-xs text-cp-text-tertiary">
            选定分组中暂无账号。
          </p>
        </div>
      </section>
    </fieldset>

    <template #footer>
      <div class="flex justify-end gap-2">
        <BaseButton @click="open = false">
          取消
        </BaseButton>
        <BaseButton variant="primary" :loading="saving" :disabled="readOnly || invalidGateways.length > 0 || !!dynamicProblem" @click="submit">
          保存设置
        </BaseButton>
      </div>
    </template>
  </BaseModal>
</template>

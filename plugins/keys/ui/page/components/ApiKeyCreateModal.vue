<script setup lang="ts">
import { enabled } from '@sdk/catalog'
import type { ApiKeyFormValue } from '../composables/useApiKeyMutations'
import type { Account, AccountGroup } from '@/api'
import { Copy, DollarSign, KeyRound, Upload } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'

import { getAccounts } from '@/api'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import KeyCapabilityNotice from '@/components/KeyCapabilityNotice.vue'
import { excelAvailable, refreshPlugins } from '@/plugins/catalog'
import PluginSlot from '@/plugins/PluginSlot.vue'
import { rotationOptions } from '@kit/rotation'

const props = defineProps<{
  groups: AccountGroup[]
  groupLoading: boolean
  editing: boolean
  createdKey: string
  saving: boolean
}>()
const emit = defineEmits<{
  save: []
  copy: [text: string]
  importCcs: []
}>()
const open = defineModel<boolean>({ default: false })
const createdOpen = defineModel<boolean>('createdOpen', { default: false })
const form = defineModel<ApiKeyFormValue>('form', { required: true })
const title = computed(() => props.editing ? '编辑密钥' : '创建 API Key')
const accounts = shallowRef<Account[]>([])
const accountLoading = shallowRef(false)
watch(open, (active) => {
  if (active)
    void refreshPlugins()
})
watch([excelAvailable, open], () => {
  // 新建草稿不保留已消失的选项；已有密钥绑定保持原值。
  if (open.value && !props.editing && !excelAvailable.value)
    form.value.excelBridgeEnabled = false
})
const accountError = shallowRef('')
const accountSearch = shallowRef('')
const filterGroup = shallowRef('')
const scopeOptions = [{ label: '固定账号', value: 'account' }, { label: '指定多个账号', value: 'accounts' }, { label: '绑定账号组', value: 'groups' }]
const strategyOptions = [{ label: '跟随全局设置', value: '' }, ...rotationOptions]
const filterOptions = computed(() => [{ label: '全部分组', value: '' }, ...props.groups.map(group => ({ label: group.name, value: group.id }))])
const filteredAccounts = computed(() => accounts.value.filter(account =>
  (!filterGroup.value || account.groups.some(group => group.id === filterGroup.value))
  && `${account.name} ${account.email}`.toLowerCase().includes(accountSearch.value.trim().toLowerCase()),
))
const effectiveAccounts = computed(() => accounts.value.filter(account => form.value.scopeMode === 'groups'
  ? account.groups.some(group => group.enabled && form.value.groupIds.includes(group.id))
  : form.value.scopeMode === 'accounts' ? form.value.accountIds.includes(account.id) : account.id === form.value.accountId))
function toggleAccount(id: string, selected: boolean) {
  form.value.accountIds = selected ? [...new Set([...form.value.accountIds, id])] : form.value.accountIds.filter(value => value !== id)
}
function selectVisible() {
  form.value.accountIds = [...new Set([...form.value.accountIds, ...filteredAccounts.value.map(account => account.id)])]
}
const accountOptions = computed(() => accounts.value.map(account => ({
  label: `${account.name || account.email || '未命名账号'} · ${account.planTypeDisplay}`,
  description: account.email || undefined,
  value: account.id,
})))
watch(open, async (active, _previous, onCleanup) => {
  if (!active)
    return
  const controller = new AbortController()
  onCleanup(() => controller.abort())
  accountLoading.value = true
  accountError.value = ''
  try {
    const loaded: Account[] = []
    for (let page = 1; ; page++) {
      const result = await getAccounts({ page, pageSize: 200, provider: 'openai' }, { signal: controller.signal })
      loaded.push(...result.items)
      if (page >= result.page.totalPages)
        break
    }
    if (!controller.signal.aborted)
      accounts.value = loaded
  }
  catch {
    if (!controller.signal.aborted)
      accountError.value = '读取账号失败，请关闭后重试'
  }
  finally {
    if (!controller.signal.aborted)
      accountLoading.value = false
  }
})
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    description="账号范围与插件仅作用于当前 Key"
    tone="info"
    size="lg"
    :dismissible="!saving"
  >
    <template #icon>
      <KeyRound class="text-cp-text" :size="20" aria-hidden="true" />
    </template>

    <BaseForm class="grid gap-6">
      <BaseFormItem label="名称" required>
        <BaseInput
          v-model="form.name"
          aria-label="名称"
          placeholder="例如：生产环境"
          :disabled="saving"
        />
      </BaseFormItem>

      <BaseFormItem label="标签（可选）">
        <BaseInput
          v-model="form.label"
          aria-label="标签（可选）"
          placeholder="例如：后端服务"
          :disabled="saving"
        />
      </BaseFormItem>

      <BaseFormItem
        v-if="!editing"
        label="自定义 Key（可选）"
      >
        <BaseInput
          v-model="form.customKey"
          type="password"
          autocomplete="new-password"
          :spellcheck="false"
          aria-label="自定义 Key（可选）"
          placeholder="留空自动生成"
          :disabled="saving"
        />
      </BaseFormItem>

      <BaseFormItem label="账号范围" required>
        <BaseSelect v-model="form.scopeMode" :options="scopeOptions" :disabled="saving" aria-label="账号范围" class="w-full" />
      </BaseFormItem>
      <BaseFormItem v-if="form.scopeMode === 'account'" label="固定账号" required>
        <BaseSelect
          v-model="form.accountId" :options="accountOptions"
          placeholder="请选择账号" :disabled="saving || accountLoading"
          class="w-full" searchable wrap-options
        />
        <p v-if="accountError" class="text-cp-xs text-cp-error-text">
          {{ accountError }}
        </p>
      </BaseFormItem>

      <BaseFormItem v-if="form.scopeMode === 'accounts'" label="选择账号" required>
        <div class="grid gap-3">
          <div class="grid gap-2 sm:grid-cols-2">
            <BaseSelect id="key-account-group-filter" v-model="filterGroup" :options="filterOptions" :disabled="saving" aria-label="按组筛选账号" class="min-w-0 w-full" searchable wrap-options />
            <BaseInput id="key-account-search" v-model="accountSearch" :disabled="saving" aria-label="搜索账号" placeholder="搜索名称或邮箱" />
          </div>
          <div class="flex justify-between text-cp-xs text-cp-text-secondary">
            <span>已选择 {{ form.accountIds.length }} 个账号</span>
            <BaseButton size="sm" :disabled="saving || accountLoading" @click="selectVisible">
              选择筛选结果
            </BaseButton>
          </div>
          <div class="grid max-h-52 gap-3 overflow-y-auto rounded-cp bg-cp-fill-quaternary p-3">
            <div v-for="account in filteredAccounts" :key="account.id" class="flex items-center gap-3">
              <BaseCheckbox :model-value="form.accountIds.includes(account.id)" :label="account.name" :disabled="saving" @update:model-value="toggleAccount(account.id, $event)" />
              <div class="min-w-0 flex-1">
                <div class="whitespace-normal wrap-anywhere text-cp text-cp-text">
                  {{ account.name }} · {{ account.planTypeDisplay }}
                </div>
                <div class="truncate text-cp-xs text-cp-text-secondary">
                  {{ account.email }} · {{ account.enabled ? '已启用' : '已停用' }}
                </div>
              </div>
            </div>
            <span v-if="!filteredAccounts.length" class="text-cp-xs text-cp-text-secondary">没有匹配账号</span>
          </div>
          <span class="text-cp-xs text-cp-text-secondary">保存固定名单，之后分组增减成员不会改变本 Key 的范围</span>
        </div>
      </BaseFormItem>
      <BaseFormItem v-if="form.scopeMode === 'groups'" label="选择账号组" required>
        <AccountGroupCheckboxGrid v-model="form.groupIds" :groups="groups" :loading="groupLoading" :disabled="saving" />
        <p class="mt-2 text-cp-xs text-cp-text-secondary">
          跟随分组成员变化，重复账号只计一次，当前匹配 {{ effectiveAccounts.length }} 个 OpenAI 账号
        </p>
      </BaseFormItem>
      <BaseFormItem label="轮转方式">
        <BaseSelect v-model="form.rotationStrategy" :options="strategyOptions" :disabled="saving" aria-label="轮转方式" class="w-full" />
        <p class="mt-2 text-cp-xs text-cp-text-secondary">
          新会话按策略选号，已有会话优先保持账号，Native 额度耗尽时仅在历史完整且尚未交付结果的条件下恢复
        </p>
      </BaseFormItem>
      <p v-if="accountError" role="alert" class="text-cp-xs text-cp-error-text">
        {{ accountError }}
      </p>
      <BaseFormItem v-if="excelAvailable" label="当前 Key 的 Excel 插件">
        <BaseSwitch v-model="form.excelBridgeEnabled" label="启用 Excel Bridge" :disabled="saving" />
        <p class="text-cp-xs text-cp-text-secondary">
          关闭走 Native，开启走 Excel Bridge，同账号其他 Key 不改变
        </p>
        <p v-if="form.excelBridgeEnabled && effectiveAccounts.some(account => account.authenticationKind !== 'oauth')" class="text-cp-xs text-cp-error-text">
          Excel Bridge 需要 Codex OAuth 账号
        </p>
      </BaseFormItem>

      <PluginSlot placement="key-editor" />
      <div class="grid gap-6 sm:grid-cols-2">
        <KeyCapabilityNotice v-if="!form.excelBridgeEnabled || excelAvailable" :excel-enabled="form.excelBridgeEnabled" class="sm:col-span-2" />
        <BaseFormItem label="日限额">
          <BaseInput
            v-model="form.dailyLimitUsd"
            type="number"
            min="0"
            step="any"
            aria-label="日限额（美元）"
            placeholder="不限制"
            :disabled="saving"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </BaseInput>
        </BaseFormItem>
        <BaseFormItem label="周限额">
          <BaseInput
            v-model="form.weeklyLimitUsd"
            type="number"
            min="0"
            step="any"
            aria-label="周限额（美元）"
            placeholder="不限制"
            :disabled="saving"
          >
            <template #prefix>
              <DollarSign class="size-4" aria-hidden="true" />
            </template>
          </BaseInput>
        </BaseFormItem>
      </div>

      <div class="grid gap-6 sm:grid-cols-2">
        <BaseFormItem label="最大并发">
          <BaseInput
            v-model="form.maxConcurrency"
            type="number"
            aria-label="最大并发"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
          />
        </BaseFormItem>
        <BaseFormItem label="每分钟请求数（RPM）">
          <BaseInput
            v-model="form.requestsPerMinute"
            type="number"
            aria-label="每分钟请求数（RPM）"
            min="0"
            step="1"
            placeholder="不限制"
            :disabled="saving"
          />
        </BaseFormItem>
      </div>
    </BaseForm>

    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!form.name.trim()"
        @click="emit('save')"
      >
        {{ editing ? '保存更改' : '创建' }}
      </BaseButton>
    </template>
  </BaseModal>

  <BaseModal
    v-model="createdOpen"
    title="API Key 已创建"
    description="复制密钥，或直接导入 CCSwitch"
    tone="success"
    size="md"
  >
    <div class="flex flex-col gap-4">
      <div class="rounded-cp border border-cp-warning-border bg-cp-warning-container px-4 py-3">
        <p class="m-0 text-cp font-semibold text-cp-warning-on-container">
          该密钥具有网关访问权限，请仅发送给可信调用方
        </p>
      </div>
      <div>
        <p class="mb-2 text-cp font-medium text-cp-text-secondary">
          API Key
        </p>
        <div class="flex items-center gap-2">
          <code class="flex-1 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 font-mono text-cp break-all text-cp-text">
            {{ createdKey }}
          </code>
          <BaseIconButton size="md" label="复制" @click="emit('copy', createdKey)">
            <Copy class="size-4" />
          </BaseIconButton>
        </div>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" @click="emit('copy', createdKey)">
        <template #icon>
          <Copy class="size-4" />
        </template>
        复制密钥
      </BaseButton>
      <BaseButton v-if="enabled('client-access')" variant="secondary" @click="emit('importCcs')">
        <template #icon>
          <Upload class="size-4" />
        </template>
        导入 CCSwitch
      </BaseButton>
      <BaseButton variant="primary" @click="createdOpen = false">
        我已保存
      </BaseButton>
    </template>
  </BaseModal>
</template>

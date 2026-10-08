<script setup lang="ts">
import { Save, Undo2 } from '@lucide/vue'
import { computed, onScopeDispose, reactive, shallowRef, watch } from 'vue'
import { context, events, host } from '@sdk/ui'
const settingsShell = context().settingsShell === true
import { useRoute } from 'vue-router'
import BaseButton from '@/components/base/BaseButton.vue'
import SettingsSectionNav from '@kit/SettingsSectionNav.vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import AccountAutoFreezeCard from './components/AccountAutoFreezeCard.vue'

import ClientProfileCard from './components/ClientProfileCard.vue'
import InferenceLimitsCard from './components/InferenceLimitsCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RequestLocationCard from './components/RequestLocationCard.vue'
import RequestQueueCard from './components/RequestQueueCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'
import SettingsAccessSection from './components/SettingsAccessSection.vue'
import TokenRefreshCard from './components/TokenRefreshCard.vue'
import { useSettingsForm } from './composables/useSettingsForm'
import { rotationOptions } from '@kit/rotation'

const route = useRoute()
const section = computed(() => {
  switch (route.name) {
    case 'settings-upstream': return 'upstream'
    case 'settings-access': return 'access'
    default: return 'runtime'
  }
})
const isBasicSection = computed(() => !['pricing', 'backup', 'plugins'].includes(section.value))
const visited = reactive(new Set<string>())
const settingsVisited = shallowRef(false)

const {
  savingLocation,
  locationStatus,
  saveLocation,
  loading,
  saving,
  hasChanges,
  resetSettings,
  error,
  form,
  mappings,
  addMapping,
  updateMapping,
  removeMapping,
  refreshMarginSecondsValue,
  refreshConcurrencyValue,
  maxConcurrentPerAccountValue,
  requestIntervalMsValue,
  maxWaitingPerKeyValue,
  maxWaitingPerAccountValue,
  concurrencyWaitTimeoutSecondsValue,
  gatewayMaxRequestsValue,
  gatewayMaxBodyMiBValue,
  gatewayBodyBudgetMiBValue,
  accountAutoFreezeThresholdValue,
  accountAutoFreezeWindowSecondsValue,
  accountAutoFreezeDurationSecondsValue,

  minCodexDesktopVersionError,
  minCodexCliVersionError,
  saveSettings,
  loadSettings,
} = useSettingsForm()

const disabled = computed(() => saving.value || savingLocation.value || loading.value || !!error.value)

watch(section, (value) => {
  visited.add(value)
  if (isBasicSection.value && !settingsVisited.value) {
    settingsVisited.value = true
    void loadSettings()
  }
}, { immediate: true })
if (settingsShell) {
  watch(() => ({ changed: hasChanges.value, saving: saving.value, saveDisabled: loading.value || savingLocation.value || saving.value || !hasChanges.value || !!error.value, resetDisabled: saving.value || savingLocation.value || loading.value }), value => {
    void host('settings-actions', value).catch(() => {})
  }, { immediate: true })
  const handleAction = (event: Event) => {
    const action = (event as CustomEvent).detail
    if (loading.value || saving.value || savingLocation.value || context().active === false) return
    if (action === 'save' && hasChanges.value && !error.value) void saveSettings()
    if (action === 'reset') resetSettings()
  }
  events().addEventListener('plugin-settings-action', handleAction)
  onScopeDispose(() => events().removeEventListener('plugin-settings-action', handleAction))
}
</script>

<template>
  <div class="w-full">
    <SettingsSectionNav>
      <div v-if="isBasicSection || hasChanges" class="ml-auto flex items-center justify-end gap-2">
        <span v-if="hasChanges" class="mr-1 size-1.5 shrink-0 rounded-full bg-cp-warning" aria-hidden="true" />
        <BaseIconButton v-if="hasChanges" label="撤销全部基础设置更改" variant="filled" :disabled="saving || savingLocation || loading" @click="resetSettings">
          <Undo2 class="size-4" />
        </BaseIconButton>
        <BaseButton variant="primary" :loading="saving" :disabled="loading || savingLocation || !hasChanges || !!error" @click="saveSettings">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存基础设置' }}
        </BaseButton>
      </div>
    </SettingsSectionNav>

    <div v-if="settingsVisited" v-show="isBasicSection" :class="settingsShell ? 'grid w-full gap-5' : 'mt-5 grid w-full gap-5'">
      <div v-if="error" role="alert" class="flex flex-wrap items-center justify-between gap-3 rounded-cp-card bg-cp-error-container p-5 text-cp-error-on-container">
        <p class="m-0 text-cp">
          设置加载失败：{{ error }}
        </p>
        <BaseButton :loading="loading" @click="loadSettings()">
          重新加载
        </BaseButton>
      </div>

      <SettingsAccessSection
        v-if="visited.has('access')"
        v-show="section === 'access'"
        v-model:min-codex-desktop-version="form.minCodexDesktopVersion"
        v-model:min-codex-cli-version="form.minCodexCliVersion"
        :disabled="disabled"
        :loading="loading"
        :desktop-error="minCodexDesktopVersionError"
        :cli-error="minCodexCliVersionError"
      />

      <fieldset v-show="section !== 'access'" :disabled="disabled" class="m-0 grid min-w-0 gap-5 border-0 p-0" aria-label="基础设置">
        <template v-if="section === 'runtime'">
          <InferenceLimitsCard
            v-model:max-requests="gatewayMaxRequestsValue"
            v-model:max-body-mi-b="gatewayMaxBodyMiBValue"
            v-model:body-budget-mi-b="gatewayBodyBudgetMiBValue"
          />
          <RuntimeSettingsCard
            v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
            v-model:request-interval-ms="requestIntervalMsValue"
          />
          <RotationStrategyCard v-model="form.rotationStrategy" :options="rotationOptions" />
          <RequestQueueCard
            v-model:max-waiting-per-key="maxWaitingPerKeyValue"
            v-model:max-waiting-per-account="maxWaitingPerAccountValue"
            v-model:concurrency-wait-timeout-seconds="concurrencyWaitTimeoutSecondsValue"
          />
          <AccountAutoFreezeCard
            v-model:enabled="form.accountAutoFreezeEnabled"
            v-model:threshold="accountAutoFreezeThresholdValue"
            v-model:window-seconds="accountAutoFreezeWindowSecondsValue"
            v-model:duration-seconds="accountAutoFreezeDurationSecondsValue"
            v-model:probe-enabled="form.accountAutoFreezeProbeEnabled"
            v-model:probe-model="form.accountAutoFreezeProbeModel"
            v-model:adaptive-concurrency="form.accountAutoFreezeAdaptiveConcurrency"
          />
        </template>

        <div v-if="visited.has('upstream')" v-show="section === 'upstream'" class="grid min-w-0 gap-5">
          <TokenRefreshCard v-model:refresh-margin-seconds="refreshMarginSecondsValue" v-model:refresh-concurrency="refreshConcurrencyValue" />
          <ClientProfileCard
            v-model:openai="form.openaiClientProfile"
            v-model:xai="form.xaiClientProfile"
            :active="section === 'upstream'"
            :disabled="disabled"
          />
          <RequestLocationCard v-model="form.requestLocation" v-model:enabled="form.requestLocationEnabled" :disabled="saving || loading || !!error" />
          <p class="mt-2 text-cp-sm text-cp-text-secondary" role="status">
            {{ locationStatus }}
            <button v-if="locationStatus.includes('失败')" type="button" class="ml-2 text-cp-primary-text" @click="saveLocation">
              重试保存
            </button>
          </p>
          <ModelAliasesCard
            :mappings="mappings"
            :loading="loading"
            @add-mapping="addMapping"
            @update-mapping="updateMapping"
            @remove-mapping="removeMapping"
          />
        </div>
      </fieldset>
    </div>

  </div>
</template>

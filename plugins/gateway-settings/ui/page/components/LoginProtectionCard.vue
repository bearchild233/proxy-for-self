<script setup lang="ts">
import type { LoginProtectionPolicy, LoginProtectionStatus } from '@/api/modules/system'
import { usePluginPolling } from '@sdk/polling'
import { onMounted, ref } from 'vue'
import { getLoginProtection, saveLoginProtection, unbanLogin } from '@/api/modules/system'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import { toast } from '@/components/base/BaseToast'
import { formatDateTime } from '@/utils/date'

const status = ref<LoginProtectionStatus>()
const forms = ref<(LoginProtectionPolicy & { windowMinutes: string, banMinutes: string, maxBanHours: string, failures: string })[]>([])
const busy = ref(false)
const error = ref('')
let savedForms = ''
let generation = 0

function apply(value: LoginProtectionStatus) {
  status.value = value
  forms.value = value.policies.map(policy => ({ ...policy, failures: String(policy.maxFailures), windowMinutes: String(policy.windowSeconds / 60), banMinutes: String(policy.banSeconds / 60), maxBanHours: String(policy.maxBanSeconds / 3600) }))
  savedForms = JSON.stringify(forms.value)
}
async function run(action: () => Promise<LoginProtectionStatus>) {
  if (busy.value)
    return
  busy.value = true
  generation++
  error.value = ''
  try {
    apply(await action())
  }
  catch {
    error.value = '登录防护操作失败，请刷新状态后重试'
  }
  finally {
    busy.value = false
  }
}
async function save(form: typeof forms.value[number]) {
  const policy = { site: form.site, maxFailures: Number(form.failures), windowSeconds: Number(form.windowMinutes) * 60, banSeconds: Number(form.banMinutes) * 60, maxBanSeconds: Number(form.maxBanHours) * 3600 }
  if (!Object.values(policy).filter(value => typeof value === 'number').every(Number.isInteger)
    || policy.maxFailures < 3 || policy.maxFailures > 20 || policy.windowSeconds < 60 || policy.windowSeconds > 3600
    || policy.banSeconds < 60 || policy.banSeconds > 86400 || policy.maxBanSeconds < policy.banSeconds || policy.maxBanSeconds > 604800) {
    toast.error('请填写有效阈值；累计封禁上限不能小于首次封禁时间')
    return
  }
  await run(() => saveLoginProtection(policy))
  if (!error.value)
    toast.success('登录防护已保存，新封禁按新阈值执行')
}
usePluginPolling(async () => {
  if (busy.value)
    return
  const current = generation
  const result = await getLoginProtection({ silent: true })
  if (busy.value || current !== generation)
    return
  if (JSON.stringify(forms.value) === savedForms)
    apply(result)
  else
    status.value = result
}, ref(30_000))
onMounted(() => run(getLoginProtection))
</script>

<template>
  <BaseCard title="登录防护" description="按来源 IP 限制登录；重复失败延长封禁，已有会话和模型请求继续使用。">
    <template #actions>
      <BaseButton size="sm" :loading="busy" @click="run(getLoginProtection)">
        刷新
      </BaseButton>
    </template>
    <p v-if="error" role="alert" class="text-cp-error-text">
      {{ error }}
    </p>
    <p v-if="status && !status.available" class="text-cp-text-secondary">
      此部署尚未配置登录防护。
    </p>
    <template v-if="status?.available">
      <p v-if="status.healthy" class="text-cp-sm text-cp-success-text">
        运行正常
      </p>
      <p v-if="!status.healthy" role="alert" class="text-cp-error-text">
        封禁服务异常，请联系维护者检查。
      </p>
      <div class="grid gap-4">
        <form v-for="form in forms" :key="form.site" class="grid gap-3 rounded-cp bg-cp-fill-quaternary p-3" @submit.prevent="save(form)">
          <div class="flex items-center justify-between gap-2">
            <strong>{{ form.site === 'api' ? 'API 控制台' : 'VPS 面板' }}</strong>
            <BaseButton type="submit" size="sm" :disabled="busy">
              保存
            </BaseButton>
          </div>
          <div class="grid gap-3 sm:grid-cols-4">
            <BaseFormItem label="失败次数">
              <BaseInput :id="`login-${form.site}-failures`" v-model="form.failures" aria-label="失败次数" type="number" min="3" max="20" step="1" />
            </BaseFormItem>
            <BaseFormItem label="统计窗口（分钟）">
              <BaseInput :id="`login-${form.site}-windowMinutes`" v-model="form.windowMinutes" aria-label="统计窗口（分钟）" type="number" min="1" max="60" step="1" />
            </BaseFormItem>
            <BaseFormItem label="首次封禁（分钟）">
              <BaseInput :id="`login-${form.site}-banMinutes`" v-model="form.banMinutes" aria-label="首次封禁（分钟）" type="number" min="1" max="1440" step="1" />
            </BaseFormItem>
            <BaseFormItem label="累计上限（小时）">
              <BaseInput :id="`login-${form.site}-maxBanHours`" v-model="form.maxBanHours" aria-label="累计上限（小时）" type="number" min="1" max="168" step="1" />
            </BaseFormItem>
          </div>
        </form>
      </div>
      <p class="mb-2 text-cp-sm text-cp-text-secondary">
        当前封禁 {{ status.totalBans }} 个来源；最多展示最近 100 条。VPS 面板自身的 15 分钟保护可能仍需等待。
      </p>
      <div v-for="ban in status.bans" :key="`${ban.site}-${ban.ip}`" class="flex flex-wrap items-center justify-between gap-2 border-t border-cp-border-secondary py-2 text-cp-sm">
        <span>{{ ban.site === 'api' ? 'API' : 'VPS' }} · <span class="font-mono">{{ ban.ip }}</span> · {{ formatDateTime(ban.expiresAt * 1000) }} 到期</span>
        <BaseButton size="sm" :disabled="busy" @click="run(() => unbanLogin(ban.site, ban.ip))">
          解封
        </BaseButton>
      </div>
    </template>
  </BaseCard>
</template>

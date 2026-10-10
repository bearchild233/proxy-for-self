<script setup lang="ts">
import type { AccountCreateForm } from './model'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

defineProps<{ disabled: boolean, showName?: boolean }>()
const form = defineModel<AccountCreateForm>({ required: true })
</script>

<template>
  <div class="grid gap-4">
    <BaseFormItem v-if="showName !== false" label="账号名称">
      <BaseInput v-model="form.name" :maxlength="100" :disabled="disabled" aria-label="账号名称" placeholder="例如：主力 Plus；留空使用导入文件或账号默认名称" />
      <p v-if="form.provider === 'batch' || form.mode === 'json' || form.mode === 'access_token' || form.mode === 'refresh_token'" class="mt-1 mb-0 text-cp-xs text-cp-text-quaternary">
        填写后应用于本次导入的所有账号
      </p>
    </BaseFormItem>
    <BaseFormItem label="备注">
      <BaseTextarea v-model="form.notes" :rows="2" :maxlength="500" placeholder="最多 500 字，可不填" :disabled="disabled" />
    </BaseFormItem>
  </div>
</template>

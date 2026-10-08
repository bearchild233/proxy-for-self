<script setup lang="ts">
import { enabled } from '@sdk/catalog'
import { host } from '@sdk/ui'
import { watch } from 'vue'

const props = defineProps<{ title?: string, apiKey: { name?: string, key?: string, routing?: { mode: string } | null, boundAccountId?: string | null, excelBridgeEnabled?: boolean } | null, apiBaseUrl: string }>()
defineEmits<{ copy: [text: string] }>()
const open = defineModel<boolean>({ default: false })
watch(open, async (value) => {
  if (!value)
    return
  try {
    if (enabled('client-access'))
      await host('open', { id: 'client-access', context: { apiKey: props.apiKey, apiBaseUrl: props.apiBaseUrl, title: props.title } })
  }
  finally { open.value = false }
})
</script>

<template>
  <span hidden />
</template>

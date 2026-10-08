<script setup lang="ts">
import { ref, watch } from 'vue'
import { context, host } from '@sdk/ui'
import ConfigPanel from './ConfigPanel.vue'
const ctx = context() as unknown as { apiBaseUrl: string, apiKey: { name?: string, key?: string, routing?: { mode: string } | null, boundAccountId?: string | null, excelBridgeEnabled?: boolean } }
const open = ref(true)
watch(open, value => { if (!value) void host('close', {}) })
function copy(text: string) { void host('copy', { text }) }
</script>
<template><ConfigPanel v-model="open" title="导入客户端配置" :api-key="ctx.apiKey" :api-base-url="ctx.apiBaseUrl" @copy="copy" /></template>

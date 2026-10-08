<script setup lang="ts">
import { computed } from 'vue'
import { activePlugins } from './catalog'
import { host } from './ui'
const props = defineProps<{ placement: string, pluginId?: string, context?: Record<string, unknown> }>()
const pages = computed(() => activePlugins.value.filter(p => !props.pluginId || p.id === props.pluginId).flatMap(p => p.pages.filter(page => page.slot === props.placement).map(page => ({ ...page, pluginId: p.id }))))
</script>
<template><div v-if="pages.length" class="flex flex-wrap gap-2"><button v-for="page in pages" :key="page.pluginId + page.id" type="button" class="rounded-cp bg-cp-fill-tertiary px-3 py-2" @click="host('open', { id: page.pluginId, pageId: page.id, context: props.context })">{{ page.title }}</button></div></template>

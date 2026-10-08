<script setup lang="ts">
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseTag from '@/components/base/BaseTag.vue'
</script>
<template>
  <section class="space-y-5 p-5 text-cp-text">
    <h2 class="text-cp-xl font-emphasis">主题预览</h2>
    <div class="grid gap-4 md:grid-cols-3">
      <BaseCard v-for="item in [{label:'请求',value:'1,280'},{label:'缓存命中',value:'86.4%'},{label:'平均延迟',value:'820 ms'}]" :key="item.label"><div class="p-5"><p class="text-cp-sm text-cp-text-secondary">{{ item.label }}</p><p class="mt-3 text-2xl font-emphasis">{{ item.value }}</p></div></BaseCard>
    </div>
    <div class="rounded-cp-card bg-cp-bg-container p-5"><p class="mb-4 font-emphasis">组件与状态</p><div class="flex flex-wrap gap-3"><BaseButton variant="primary">主要操作</BaseButton><BaseButton variant="secondary">次要操作</BaseButton><BaseTag tone="success">正常</BaseTag><BaseTag tone="warning">待处理</BaseTag></div><div class="mt-5 h-2 overflow-hidden rounded-full bg-cp-fill-tertiary"><div class="h-full w-2/3 bg-cp-primary" /></div><p class="mt-4 text-cp-sm text-cp-text-tertiary">示例数据，用于观察颜色、文字与组件对比度。</p></div>
  </section>
</template>

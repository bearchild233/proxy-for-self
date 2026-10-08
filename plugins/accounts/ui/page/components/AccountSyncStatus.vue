<script setup lang="ts">
defineProps<{ lastRefreshedAt: string, message: string }>()
</script>

<template>
  <!-- 同步提示始终占用相同空间，状态变化不挤动标题、卡片或列表。 -->
  <div class="grid h-10 w-80 shrink-0 content-center text-cp-xs text-cp-text-secondary" role="status">
    <span class="truncate leading-5 tabular-nums">当前页每 30 秒同步 · {{ lastRefreshedAt || '尚未同步上游' }}</span>
    <span class="h-5 truncate leading-5" :title="message">{{ message }}</span>
  </div>
</template>

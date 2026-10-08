<script setup lang="ts">
import SettingsSectionNav from '@kit/SettingsSectionNav.vue'
import SettingsBackupSection from './SettingsBackupSection.vue'
</script>
<template>
  <div class="w-full">
    <SettingsSectionNav />
    <SettingsBackupSection active />
  </div>
</template>

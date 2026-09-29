<script setup lang="ts">
import type { RequestLocation } from '@/api'
import { computed } from 'vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { requestLocations } from '@/utils/requestLocations'

defineProps<{ disabled?: boolean }>()
const location = defineModel<RequestLocation>({ required: true })

function update(field: keyof RequestLocation, value: string) {
  location.value = { ...location.value, [field]: value }
}
const regionNames = new Intl.DisplayNames(['zh-CN'], { type: 'region' })
function preserve(options: { label: string, value: string }[], current: string) {
  return current && !options.some(item => item.value === current)
    ? [{ label: `${current}（已保存）`, value: current }, ...options]
    : options
}
const countries = computed(() => preserve([...new Set(requestLocations.map(item => item.country))].map(value => ({ value, label: `${regionNames.of(value)} · ${value}` })), location.value.country))
const inCountry = computed(() => requestLocations.filter(item => item.country === location.value.country))
const regions = computed(() => preserve([...new Set(inCountry.value.map(item => item.region))].map(value => ({ value, label: value })), location.value.region))
const inRegion = computed(() => inCountry.value.filter(item => item.region === location.value.region))
const cities = computed(() => preserve(inRegion.value.map(item => ({ value: item.city, label: `${item.label} · ${item.city}` })), location.value.city))
const timezoneValues = [...new Set(['UTC', ...requestLocations.map(item => item.timezone), ...Intl.supportedValuesOf('timeZone')])].sort()
const timezones = computed(() => preserve(timezoneValues.map(value => ({ value, label: value.replaceAll('_', ' ') })), location.value.timezone))
function selectPlace(field: 'country' | 'region' | 'city', value: string) {
  if (location.value[field] === value)
    return
  const choices = field === 'country' ? requestLocations : field === 'region' ? inCountry.value : inRegion.value
  const selected = choices.find(item => item[field] === value)
  if (selected) {
    const { country, region, city, timezone } = selected
    location.value = { country, region, city, timezone }
  }
}
</script>

<template>
  <div class="grid gap-4 sm:grid-cols-2">
    <BaseFormItem label="国家或地区" required>
      <BaseSelect :model-value="location.country" :options="countries" :disabled="disabled" aria-label="国家或地区" @update:model-value="selectPlace('country', $event)" />
    </BaseFormItem>
    <BaseFormItem label="地区" required>
      <BaseSelect :model-value="location.region" :options="regions" :disabled="disabled" aria-label="地区" @update:model-value="selectPlace('region', $event)" />
    </BaseFormItem>
    <BaseFormItem label="城市" required>
      <BaseSelect :model-value="location.city" :options="cities" :disabled="disabled" aria-label="城市" @update:model-value="selectPlace('city', $event)" />
    </BaseFormItem>
    <BaseFormItem label="IANA 时区" required>
      <BaseSelect :model-value="location.timezone" :options="timezones" :disabled="disabled" aria-label="IANA 时区" @update:model-value="update('timezone', $event)" />
    </BaseFormItem>
    <p class="text-cp-xs text-cp-text-secondary sm:col-span-2">
      提供常用出口城市，选择城市会同步时区，也可单独调整时区
    </p>
  </div>
</template>

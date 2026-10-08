import type { Ref } from 'vue'
import { useVisiblePolling } from '@/composables/useVisiblePolling'
import { usePluginVisibility } from './visibility'

export function usePluginPolling(refresh: () => Promise<unknown>, interval: Readonly<Ref<number>>) {
  useVisiblePolling(refresh, interval, usePluginVisibility())
}

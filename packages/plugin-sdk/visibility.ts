import { useDocumentVisibility } from '@vueuse/core'
import { computed, onScopeDispose, shallowRef } from 'vue'
import { context, events } from './ui'

export function usePluginVisibility() {
  const documentVisibility = useDocumentVisibility()
  const active = shallowRef(context().active !== false)
  const update = () => { active.value = context().active !== false }
  events().addEventListener('plugin-context', update)
  onScopeDispose(() => events().removeEventListener('plugin-context', update))
  return computed(() => active.value && documentVisibility.value === 'visible' ? 'visible' : 'hidden')
}

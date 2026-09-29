import { useObjectUrl } from '@vueuse/core'
import { nextTick, shallowRef } from 'vue'

export function useDownload() {
  const object = shallowRef<Blob | null>(null)
  const objectUrl = useObjectUrl(object)

  async function downloadJson(payload: unknown, fileName: string) {
    await downloadText(`${JSON.stringify(payload, null, 2)}\n`, fileName, 'application/json;charset=utf-8')
  }

  async function downloadText(text: string, fileName: string, type = 'text/plain;charset=utf-8') {
    object.value = new Blob([text], { type })
    await nextTick()

    if (!objectUrl.value)
      return

    const link = document.createElement('a')
    link.href = objectUrl.value
    link.download = fileName
    document.body.appendChild(link)
    link.click()
    link.remove()
  }

  return {
    downloadJson,
    downloadText,
  }
}

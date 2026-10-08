import { host } from './ui'

export function useDownload() {
  async function downloadText(text: string, fileName: string, type = 'text/plain;charset=utf-8') {
    await host('download', { text, fileName, type })
  }
  async function downloadJson(payload: unknown, fileName: string) {
    await downloadText(`${JSON.stringify(payload, null, 2)}\n`, fileName, 'application/json;charset=utf-8')
  }
  return { downloadText, downloadJson }
}

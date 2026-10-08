// 同一版本的多个页面共用资源；最后一个使用者退出后才释放。
export class SharedResources<T> {
  private entries = new Map<string, { value: Promise<T>, users: number }>()

  private dispose: (key: string, value: T) => void

  constructor(dispose: (key: string, value: T) => void) {
    this.dispose = dispose
  }

  async acquire(key: string, create: () => Promise<T>) {
    let entry = this.entries.get(key)
    if (!entry) {
      entry = { value: Promise.resolve().then(create), users: 0 }
      this.entries.set(key, entry)
    }
    const current = entry
    current.users++
    let value: T
    try {
      value = await current.value
    }
    catch (error) {
      current.users--
      if (this.entries.get(key) === current)
        this.entries.delete(key)
      throw error
    }
    let released = false
    return { ...value, release: () => {
      if (released)
        return
      released = true
      if (--current.users === 0) {
        if (this.entries.get(key) === current)
          this.entries.delete(key)
        this.dispose(key, value)
      }
    } }
  }

  clear() {
    // 已挂载页面仍持有释放回调；新身份不能接管它们的租约。
    this.entries.clear()
  }
}

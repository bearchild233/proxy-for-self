interface AccountIdentityInput {
  name?: string | null
  email?: string | null
  reference?: string | null
}

/** ID 仅用于关联与筛选；可见文本只使用名称和邮箱。 */
export function accountIdentity(input: AccountIdentityInput) {
  const reference = input.reference?.trim()
  const name = input.name?.trim()
  const email = input.email?.trim() || (name?.includes('@') ? name : '')
  const primary = (name && name !== reference ? name : '') || email || '未知账号'
  const secondary = email && email !== primary ? email : ''
  return { primary, secondary, full: secondary ? `${primary} · ${secondary}` : primary }
}

export function keyCapabilities(excelEnabled: boolean) {
  return {
    channel: excelEnabled ? 'Excel Bridge' : 'Codex Native',
    summary: '使用 Responses 协议，不提供 Chat Completions 或 Gemini 协议转换',
    rows: [
      ['对话', 'POST /v1/responses · JSON 与 SSE'],
      ['模型', excelEnabled ? 'GET /v1/models · 保留原桥 12 个别名与映射' : 'GET /v1/models · 可用模型取决于绑定账号'],
      ['WebSocket', excelEnabled ? '不支持 · 使用 HTTP Responses' : 'GET /v1/responses · Native WebSocket'],
      ['图片输入', excelEnabled ? '复用原桥 · 优先内嵌，必要时上传到 OpenAI 附件接口' : '按 Native 请求原样提交，由上游决定支持范围'],
      ['生图与改图', excelEnabled ? 'POST /v1/images/generations、/edits · 原桥 gpt-image-2 与 PNG 限制，改图使用 JSON 内嵌 data: 图片' : 'POST /v1/images/generations、/edits · 原生 JSON 接口'],
      ['文件输入', excelEnabled ? '不支持 input_file · 请先提取文本或发送 input_image' : '按 Native 请求原样提交，由上游决定支持范围'],
      ['会话续接', excelEnabled ? '必须发送完整历史 · 不支持 previous_response_id' : '同一 Key 与绑定代次内保持账号亲和 · 限额后仅在完整历史和安全边界内切号恢复'],
      ['独立搜索', excelEnabled ? '不支持 /v1/alpha/search · 不影响客户端工具调用' : 'POST /v1/alpha/search · 原生独立搜索'],
      ['不支持的协议', '/v1/chat/completions 与 /v1beta/* · 明确拒绝，不静默改写或切换通道'],
    ],
    warnings: excelEnabled
      ? [
          '与原桥一致：图片上传失败或后端仍拒绝时，会改成说明文本继续请求，不保证模型实际看到了图片',
          '按当前 Key 账号范围选号，不自动回退 Native · Worker 请求封装与 JSON 响应各限 8 MiB',
          '原桥文件引用缓存仅在当前请求内复用 · 工具回放按 Key、账号和绑定代次隔离，Worker 重启后清空',
        ]
      : ['额度、模型及工具是否可用由上游账号决定 · TLS 指纹及全部 WS 边界行为尚未完成真实上游验收'],
  }
}

export function buildCapabilityReadme(excelEnabled: boolean): string {
  const info = keyCapabilities(excelEnabled)
  return [
    `${info.channel} 使用说明`,
    info.summary,
    ...info.rows.map(([label, value]) => `${label}：${value}`),
    '',
    ...info.warnings,
    '',
    '这些说明描述当前实现，不代表当前账号已经完成真实上游验收',
    '重置卡只在管理员选择账号和卡片并明确确认后使用，导出或导入配置不会兑换卡片',
    '本地 Key 预算不等于上游 Credits 或剩余额度',
  ].join('\n')
}

# appearance 插件（主题外观）

<!-- doc-sync paths: plugins/appearance -->

## 职责与边界

可选界面插件（页面 `/theme`，仅管理员，`extensions` 分类）：编辑并预览控制台主题——颜色模式、预设主色/自定义色、全局 token、组件级 token。**没有后端进程、没有后端接口**（能力只有 `auth.*`）；主题配置保存在浏览器（主壳 `theme` store 的持久化），并通过宿主操作 `theme` 回写主壳。停用插件后已保存的主题继续生效。

- 依赖：主壳的主题引擎（`frontend/src/theme/`、`frontend/src/stores/modules/theme.ts`），规则见 [主题文档](../theme.md)。允许调用宿主 `theme` 操作的只有本插件和 `usage-analytics`。

## 实现逻辑

- `ui/page/index.vue` 仅挂载 `AppearanceSettingsSection`。
- `composables/useThemeEditor.ts`：编辑范围 `global`（颜色/尺寸/样式三类）与 `component`，预览模式 `page` / `components`，维护 `ThemeEditorDraft`（模式、主色、自定义色、逐 token 覆盖），应用/重置经主壳 store。
- `composables/useThemePreviewCanvas.ts` 与 `ThemePreviewCanvas` / `ThemePreviewScope`：在隔离作用域里用同一套 `--cp-*` 变量渲染预览，不影响真实页面直到应用；预览内容来自 `ThemeDashboardPreview`（使用 `ui/page/fixtures/dashboard.ts` 的模拟数据）和 `ThemeComponentPreview`。
- 面板：`ThemeGlobalTokenPanel`、`ThemeComponentTokenPanel`，字段控件 `ThemeColorTokenField` / `ThemeNumberTokenField` / `ThemeTextTokenField`；允许 alpha 的 token 由 `themeTokenAllowsAlpha` 决定。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `plugins/appearance/plugin.json` | 清单：页面 `/theme`，2 个能力（`auth.*`） |
| `plugins/appearance/ui/entries.json` | 页面入口 |
| `plugins/appearance/ui/page/components/` | 设置区、token 面板与字段、预览画布 |
| `plugins/appearance/ui/page/composables/` | 编辑状态与预览画布 |
| `plugins/appearance/ui/page/fixtures/dashboard.ts` | 预览用的模拟数据 |

## 数据、配置与接口

无服务端数据。浏览器存储由主壳 `theme` store 管理；与宿主的接口是 `host('theme', {mode,color,customColor,customization})`。

## 改动指引

- 新增 token：先改 `frontend/src/theme/`（常量、规范化、派生、类型）和 [主题文档](../theme.md)，再加面板字段与预览。
- 预览只使用模拟数据，不请求真实接口；不要让预览样式泄漏到页面真实作用域。
- 校验：`node scripts/check-plugin.mjs appearance`、`node scripts/build-plugin.mjs appearance`。

# frontend-shell（Vue 管理主壳）

<!-- doc-sync paths: frontend/src -->

## 职责与边界

管理端 Vue 3 单页应用的**主壳**：登录、会话、路由与布局、主题、基础 UI 组件、请求层、插件页面的加载与运行环境、插件管理页和系统更新弹窗。**所有功能页面都在插件里**（`plugins/<id>/ui`），主壳不静态导入任何功能页面；插件包通过别名 `@plugins`、`@kit`、`@sdk` 复用主壳的 `@` 源码（见 `frontend/vite.config.ts`）。

- 技术栈：Vue 3 + Vue Router + Pinia（含持久化）+ Axios + Tailwind CSS 4 + ECharts，包管理 pnpm。
- 依赖：网关 `/api/auth/*`、`/api/admin/system/*`、`/api/plugins`（原生插件目录），以及 Python 控制面 `/api/plugin-platform/*`（部署时同源反代，见 [plugin-host](plugin-host.md)）。
- 公共组件、请求层、主题 token 是插件的共同基础：改它们等于改所有插件，改前看 [packages](packages.md) 与各插件文档。

## 实现逻辑

**账号政策类型**：`frontend/src/api/modules/accounts.ts` 定义可选 `quotaProtection` 与 `quotaProtectionReached`；政策保留legacy `usedPercentLimit`，新增可选 `shortTermUsedPercentLimit`、`weeklyUsedPercentLimit` 以兼容旧网关响应。更新和批量更新可携带总开关及完整整数阈值。实际分周期编辑、能力检测与保存回读由accounts插件负责，主壳不参与选号或额度判定。

**启动**（`main.ts`）：注册 `v-loading` 指令、Pinia、`useThemeStore().initializeTheme()`、路由、`authPlugin`（挂载 401 会话失效处理），挂载 `App.vue`。

**路由与登录**（`router/`）：几乎所有受保护路由（`/`、`/accounts`、`/proxies`、`/groups`、`/keys`、`/usage`、`/theme`、`/settings*`、`/extensions/:plugin/:page?`、独立的 `/key-usage`）都指向同一个 `PluginRoute.vue`，由插件页面声明的 `route` 决定显示谁；`/settings/plugins` 是主壳自带的 `PluginManager.vue`。`beforeEach`：先 `checkAuth`（本机预览 origin 下自动走 `/api/plugin-platform/local-login`），未登录跳 `/login`（带 `redirect`），Key 身份只能进 `/key-usage`，管理员访问 `/key-usage` 回仪表盘。管理员与 Key 共用 `/api/auth/*`，Auth Store 只有一份。

**请求层**（`api/request.ts`）：Axios 实例 `withCredentials`；统一信封 `{code,message,data}`，HTTP 成功但业务码失败也拒绝；`ApiError` 带 `status` `code` `requestId` `kind`；错误提示统一在拦截器弹 toast（`silent` 只关提示，不吞异常、不跳过会话失效处理）；业务码 `40101` + 401 触发一次性会话失效处理。`api/modules/*.ts` 是各接口的类型与调用封装（账号、账号预热、分组、Key、代理、用量、仪表盘、备份、定价、设置、客户端画像、系统更新等），插件直接 `import`。`accountWarmup.ts` 声明预热 read/configure/run/history/cancel、按账号状态摘要、自动来源筛选以及提示词/模型/推理强度的统一类型与前端校验；实际执行由宿主桥转发。模型强度选项遵循目录能力，未知目录交给执行时校验，明确不支持的当前值保留展示但禁选。

**插件运行环境**（`plugins/`）：
- `platform.ts`：`platformPlugins`（`/api/plugin-platform/catalog` 目录，内容未变化不触发响应式更新）、`openPlugin`（弹出式页面）；`catalog.ts`：原生插件（Excel 等）目录 `/api/plugins`。
- `PluginRoute.vue`：按当前路由选出页面，`pageCache.ts::retainPages` 保持已打开页面驻留（隐藏而不卸载，保留表单与滚动），`preloadPlan.ts` + `pageResources.ts` 静默预载常用页面的代码与样式（最多 3 个并行，不运行页面 setup、不查业务数据；逐插件 `lazyLoad` 可关闭预载）。
- `PluginFrame.vue`：加载单个页面。`loadPluginPage` 取页面包并持有版本租约（`pageResources.ts`，同版本多页共用资源，最后一个使用者退出才释放），`leaseHeartbeat.ts` 每 30 秒续期（临时网络失败不卸载页面，只有明确版本/权限拒绝才刷新目录并换到当前版本）。两种运行方式：`vue-component`（内置受信任插件，`componentResources.ts` 准备模块、`componentRuntime.ts` 提供宿主接口 v1：vue / router / pinia / 共享 UI，组件错误被 `onErrorCaptured` 收敛为“插件运行失败”）；iframe（外部插件，不透明来源 + CSP，经 postMessage 桥调用声明过的能力）。桥的 `call` 走 `/api/plugin-platform/call/<id>/<version>/<能力名>`，`host` 提供设置页保存/撤销、主题修改（仅 `appearance` 与 `usage-analytics`）、下载等少量宿主操作。
- `PluginSlot.vue`：嵌入式槽位（账号菜单、Key 配置等）的 iframe 承载；`PluginManager.vue`：插件管理（上传、启用/停用/卸载、回退、懒加载、排序、发布说明）；`SettingsHostHeader.vue` + `settingsActions.ts`：设置页统一页头与“保存/撤销”由当前活动设置页驱动；`sharedResources.ts` `echarts.ts` `auth.ts` 辅助。

**布局**（`layout/`）：`index.vue` 侧边栏 + 主区；`layout/components/AppSidebar.vue` 由插件目录生成导航，`SystemUpdateModal/` 系统更新弹窗（`stores/modules/system-update.ts` 管版本、更新详情、SSE 进度）。

侧栏对 `account-warmup` 扩展使用 FlameKindling 图标，其它扩展保留 Puzzle。`accountWarmup.ts` 的设置类型增加 `groups` 范围与 groupIds，read 暴露 `groupTargetsSupported` 和账号真实分组 ID；自动插件负责多选及批量草稿，主壳不解析成员资格或调度。

**主题**（`theme/` + `stores/modules/theme.ts` + `styles/`）：`core/` 常量、规范化、解析（预设色 + 自定义色 + 逐 token 覆盖）、token；`derive/` 由种子派生颜色角色、外观、组件级 token；`theme/runtime/browser.ts` 把解析结果写成 `--cp-*` CSS 变量；store 负责模式（亮/暗/跟随系统）、过渡动画与持久化。完整规则见 [主题文档](../theme.md)。

**其余**：`components/`（`base/` 基础组件 Base*：按钮、表格含列设置/分页/展开动画、表单、弹窗、Toast、选择器等；`charts/`；`client-profile/` 客户端画像编辑器；账号分组/服务商图标等业务无关的共用展示）、`composables/`（`usePagedQuery` / `useStablePagedQuery` 分页、`useVisiblePolling` 页面可见时轮询、`useAsyncAction`、`useSharedOrder` 顺序共享保存等）、`utils/`（日期、数字、代理连接串、Excel 模型目录、请求位置；`ccswitchImport.ts` 与 `codexConfig.ts` 只是对 `plugins/client-access/ui/utils/` 的 re-export）、`directives/loading.ts`、`views/login/` 登录页。

## 文件表

| 路径 | 职责 |
| --- | --- |
| `frontend/src/main.ts` `frontend/src/App.vue` | 入口与根组件 |
| `frontend/src/router/` | 路由表与登录守卫 |
| `frontend/src/api/` | 请求层、错误规范化、各接口模块（含 `accountWarmup.ts`） |
| `frontend/src/stores/` | Pinia：`auth` `theme` `system-update` `ui` |
| `frontend/src/plugins/` | 插件平台前端运行时（见上） |
| `frontend/src/layout/` | 侧边栏布局、系统更新弹窗 |
| `frontend/src/views/` | 登录页 |
| `frontend/src/components/` | 基础与共用组件 |
| `frontend/src/composables/` | 共用组合式函数 |
| `frontend/src/theme/` | 主题引擎 |
| `frontend/src/styles/` | 全局样式与 token CSS |
| `frontend/src/utils/` | 工具函数与静态目录 |
| `frontend/src/directives/` | `v-loading` |

## 数据、配置与接口

- 构建：`frontend/vite.config.ts`（别名、`/dev` 代理到本机网关、echarts 单独分块）；产物 `frontend/dist` 由 Rust 静态服务或反代提供。
- 浏览器存储：Pinia 持久化主题与侧边栏状态；页面租约在内存。
- 与后端的合同：[管理接口](../api.md)、[插件平台](../plugin-migration.md)；插件能力白名单在 `packages/plugin-sdk/operations.json`。
- 测试：`frontend/tests/*.test.mjs`（`node --test`，覆盖账号归档、额度显示、账号预热设置/提交、插件租约/预载/导航等），`pnpm --dir frontend lint|typecheck|build`。

## 改动指引

- 新增页面不要改主壳路由：新建或修改插件（`plugin.json` + `ui/entries.json`），页面声明 `route`；只有新的**宿主能力**（公共组件、运行时接口）才改主壳，且需要平台升级（旧主壳浏览器要刷新一次）。
- 改 `api/request.ts` 的错误处理会影响全部插件：页面和 `useAsyncAction` 不重复弹接口错误，批量部分成功等业务反馈先 `silent` 再由业务 owner 一次反馈。
- 改 `PluginFrame` / 租约 / 预载：保证“同版本租约可续接”“网络超时与 5xx 不卸载页面”“退出登录清理页面与租约”三条；对应测试在 `frontend/tests/plugin-*.test.mjs`。
- 改主题 token：同步 [主题文档](../theme.md)，并检查 `plugins/appearance` 的预览。
- 主壳不得绕过管理 API 直连 Store 等；不在组件里重新解析上游错误码或正文。

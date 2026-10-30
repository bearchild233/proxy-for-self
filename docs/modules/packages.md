# packages（插件共享包）

<!-- doc-sync paths: packages -->

## 职责与边界

两个与具体插件无关的共享包，由主壳与插件构建共同使用：

- `packages/plugin-sdk`：插件与宿主之间的**合同与适配层**。包含平台策略表和能力白名单（数据文件，Python 控制面读取），以及构建插件包时替换主壳模块的“桥接实现”。
- `packages/ui-kit`：多个插件共用的展示辅助与小组件（格式化、轮转选项、账号用量窗口、套餐徽标、设置分区导航）。

两者都不是独立发布的 npm 包（`packages/package.json` 只是 `{"private":true,"type":"module"}`），通过别名使用：主壳与插件构建里 `@sdk` → `packages/plugin-sdk`，`@kit` → `packages/ui-kit`，`@plugins` → `plugins/`（`frontend/vite.config.ts`、`scripts/build-plugin.mjs`）。

## 实现逻辑

**能力白名单与策略**（数据文件）：
- `packages/plugin-sdk/operations.json`：命名能力 → 网关管理 API 的映射 `{method, path, roles}`，约 100 项，键形如 `accounts.get.admin.accounts.detail`。控制面 `Platform.call`（见 [plugin-host](plugin-host.md)）只转发这里登记且在插件清单 `capabilities` 里声明的能力，并按 `roles` 校验当前登录身份。新增页面需要的后端接口，先在这里加一行。
- `packages/plugin-sdk/policies.json`：宿主对每个内置插件的策略（`name`、`description`、`category`（basic / extensions / operations / clients / account-tools）、`required` 是否基础模块、`trustedUi` 是否允许 `vue-component`、`worker` 是否允许后台 Worker、`capabilities` / `pages` 上限）。包本身不能提升信任，基础属性由这里决定；控制面 `StaticPluginStore` 用它校验清单。

账号预热是可选的 `account-tools` 策略：`account-warmup` 页面标题为“自动预热”，保有配置、运行、历史和取消能力，以及显示分组名称的只读 `account-groups.get.admin.account-groups`；交互仅管理自动计划，手动执行位于 `accounts` 基础插件，只获显式的 `account-warmup.read/run` 桥。服务插件的方法不经过 `operations.json`，但仍必须同时出现在清单、策略和更新器 grants 中。组名元数据权限不授予服务 ownership/watch 或分组写权限。

**构建期替换**（`scripts/build-plugin.mjs` 的 `isolated-plugin-transport` 插件）：插件源码按主壳习惯写 `@/api/request`、`@/plugins/catalog`、`@/composables/useCopyText` 等，构建时这些模块被本包同名角色的文件替换：

| 主壳模块 | 替换为 | 作用 |
| --- | --- | --- |
| `frontend/src/api/request.ts` | `plugin-sdk/request.ts` | 请求改走宿主桥 `call(能力名, 参数)`，保持 `ApiError` / `silent` 语义；`setUnauthorizedHandler` 为空实现（会话由宿主处理） |
| `frontend/src/plugins/catalog.ts` | `plugin-sdk/catalog.ts` | 插件目录改为宿主上下文 / `host('catalog')`；`pluginRpc` 经宿主 |
| `frontend/src/composables/useCopyText.ts` `frontend/src/composables/useDownload.ts` | `copy.ts` `download.ts` | 复制与下载改走宿主操作 |
| `frontend/src/utils/serviceUrl.ts` `frontend/src/api/modules/client-models.ts` | `service-url.ts` `client-models.ts` | 服务根地址取宿主上下文；模型列表走 `client.models` 能力 |
| `frontend/src/plugins/PluginSlot.vue` | `plugin-sdk/PluginSlot.vue` | 槽位组件改由宿主承载 |
| `plugin-sdk/ui.ts`（仅 `vue-component`） | `plugin-sdk/component-ui.ts` | 直接使用宿主传入的 `PluginBridge`（`context` / `events` / `host` / `call`） |

`vue-component` 插件还把 `vue` `vue-router` `pinia`、`components/base/*`、`stores/modules/auth|theme` 标为外部依赖，运行时由主壳 `componentRuntime` 提供，插件包内不重复打包 Vue。iframe 运行方式使用 `bootstrap.ts::mount`：在 iframe 内创建独立应用（内存路由、主题与登录态同步、`host('ready')`），`navigation.ts` 把跨页导航交给宿主。`polling.ts`、`visibility.ts` 提供按页面可见性暂停的轮询。

**ui-kit**：`format.ts`（百分比、时长、美元、轴刻度等展示格式化）、`rotation.ts`（轮转策略选项）、`SettingsSectionNav.vue`（设置页分区导航）、`accounts/`（`AccountUsageWindow/` 账号用量窗口与请求时间线、`AccountPlanBadge.vue` 套餐徽标、`visualTone.ts` 无语义身份的稳定色调、`types.ts` 额度窗口类型）。

## 文件表

| 文件 | 职责 |
| --- | --- |
| `packages/plugin-sdk/operations.json` `packages/plugin-sdk/policies.json` | 能力白名单、宿主策略表 |
| `packages/plugin-sdk/ui.ts` `component-ui.ts` `component-modules.d.ts` | 宿主桥类型与 `context/host/call` |
| `packages/plugin-sdk/request.ts` `catalog.ts` `copy.ts` `download.ts` `service-url.ts` `client-models.ts` `PluginSlot.vue` | 构建期替换用的桥接实现 |
| `packages/plugin-sdk/bootstrap.ts` `navigation.ts` `layout.css` | iframe 运行方式的挂载、导航与布局 |
| `packages/plugin-sdk/polling.ts` `visibility.ts` | 页面可见性与轮询 |
| `packages/ui-kit/` | 共用展示辅助与组件 |

## 数据、配置与接口

无存储。对外合同：宿主桥（`context` 含 `route` / `theme` / `themeConfiguration` / `presentation` / `apiBaseUrl` / `settingsShell`；`host` 操作见 `frontend/src/plugins/PluginFrame.vue` 的 `hostOperation`：`ready` `failed` `catalog` `navigate` `open` `close` `copy` `download` `backup-download` `external` `theme` `settings-actions`）、`operations.json` / `policies.json` 的字段。

## 改动指引

- 新增后端接口给插件用：`operations.json` 加映射 → 插件 `plugin.json` 声明 → 控制面无需改代码；纯 UI 包可直接热更新，但**平台缺少该能力时必须先升级平台**，不能删清单声明绕过校验。
- 新增宿主操作：同时改 `frontend/src/plugins/PluginFrame.vue`（`hostOperation`）与本包的 `ui.ts` / `component-ui.ts` 类型；这是平台升级。
- 新增需要替换的主壳模块：在 `scripts/build-plugin.mjs` 的 `replacements` 表登记，并保持两端导出签名一致。
- 服务插件新增页面管理能力 `<插件ID>.<方法>`（不经 `operations.json`，由控制面转给插件进程）时，内置插件仍要把能力名加进 `policies.json` 对应插件的 `capabilities`，否则安装时被“能力超出平台策略”拒绝；这同样是平台升级。
- `policies.json` 变更要同步控制面部署包（`scripts/package-plugin-platform.py`）和 [插件平台](../plugin-migration.md) 的插件分类表。
- 校验：`node scripts/check-plugin.mjs <插件>`、`node scripts/build-plugin.mjs <插件>`；`frontend/plugin-tests/component-packages.test.mjs` 覆盖组件包约束。

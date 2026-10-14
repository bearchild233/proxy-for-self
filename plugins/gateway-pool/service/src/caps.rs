//! 内核能力的类型化封装。插件进程只允许 AF_UNIX，对外访问（账号、额度、上游探测、池服务）
//! 全部经内核能力完成；这里只负责参数、超时和结果解析，业务规则在 `routing`。
//!
//! 申请的能力：`accounts.read`、`quota.read`、`upstream.probe`、`http.fetch`、`log`
//! （`groups.move` 留到 011，随移组一起申请）。

use std::time::Duration;

use plugin_sdk::{Kernel, RpcResult, protocol::*};
use serde_json::Value;

/// 插件向内核申请的能力（写入握手；`plugin.json` 的 `service` 段在 012 打包时再加）。
pub const CAPABILITIES: [&str; 5] = [
    "accounts.read",
    "quota.read",
    "upstream.probe",
    "http.fetch",
    "log",
];

/// 本插件声明的钩子。
pub const HOOKS: [&str; 5] = [
    "route.select",
    "route.prepare",
    "response.head",
    "request.finish",
    "admin.call",
];

/// 读取类能力（账号、额度）的调用预算。
// TODO(design): 任务说明没有给出能力调用的超时；此处取 5 秒，需确认。
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// 额度快照中与网关池有关的事实。
pub struct QuotaView {
    /// 内核有该账号的额度快照。
    pub present: bool,
    /// 快照判定额度已耗尽。
    pub exhausted: bool,
    /// 调度信号里的剩余额度排名（0 表示最低）。
    pub rank: Option<u64>,
}

#[derive(Clone)]
pub struct Caps {
    kernel: Kernel,
}

impl Caps {
    pub fn new(kernel: Kernel) -> Self {
        Self { kernel }
    }

    pub async fn accounts(&self, ids: &[String]) -> RpcResult<Vec<AccountSummary>> {
        let result: AccountsReadResult = self
            .kernel
            .call(
                "accounts.read",
                &AccountsReadParams {
                    ids: Some(ids.to_vec()),
                },
                READ_TIMEOUT,
            )
            .await?;
        Ok(result.accounts)
    }

    pub async fn quota(&self, account_id: &str) -> RpcResult<QuotaView> {
        let result: QuotaReadResult = self
            .kernel
            .call(
                "quota.read",
                &QuotaReadParams {
                    account_id: account_id.to_owned(),
                },
                READ_TIMEOUT,
            )
            .await?;
        let snapshot = &result.snapshot;
        Ok(QuotaView {
            present: !snapshot["quota"].is_null(),
            exhausted: snapshot["quota"]["quota"]["access"] == "exhausted",
            rank: snapshot["schedulingSignals"]["remainingRank"].as_u64(),
        })
    }

    /// 探测请求的外层等待比内核侧超时多 1 秒，让内核先返回 `timeout` 结果而不是连接层超时。
    // TODO(design): 外层多等 1 秒是推断值。
    pub async fn probe(&self, params: UpstreamProbeParams) -> RpcResult<UpstreamProbeResult> {
        let budget = Duration::from_millis(params.timeout_ms) + Duration::from_secs(1);
        self.kernel.call("upstream.probe", &params, budget).await
    }

    pub async fn fetch(&self, params: HttpFetchParams) -> RpcResult<HttpFetchResult> {
        let budget = Duration::from_millis(params.timeout_ms) + Duration::from_secs(1);
        self.kernel.call("http.fetch", &params, budget).await
    }

    /// 写一条 warn 日志；日志失败不影响业务。
    pub async fn warn(&self, message: &str, fields: Value) {
        let _ = self
            .kernel
            .log(
                &LogParams {
                    level: "warn".into(),
                    message: message.to_owned(),
                    fields,
                },
                READ_TIMEOUT,
            )
            .await;
    }
}

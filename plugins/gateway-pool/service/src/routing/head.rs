//! `response.head` 与 `request.finish`：业务响应头的 21/29 判定，以及租约与访问计时的收尾。
//! 对应旧路径的 `GatewayPoolGuard::observe`、`finish` 与 `GatewayPoolAttempt::send` 里的取舍。

use std::sync::Arc;

use plugin_sdk::{RpcResult, protocol::*};
use serde_json::{Value, json};

use super::{Pool, PoolFailure};
use crate::{
    cookie::{cookie_gateway, merge_cookie},
    error::Failure,
};

/// `businessResult` 落库之后租约的去留，以及对内核的答复。
enum Next {
    /// 租约保持到整个客户流结束；没有响应时还要把接管事实落库。
    Keep { persist: bool },
    /// 租约在同一事务内归还，并按此答复内核。
    Finish(ResponseHeadResult),
}

/// 由响应头事实和 `businessResult` 的返回值决定去留。这个函数同时在事务内（决定是否归还）和事务后
/// （决定答复）使用，两处必须得到同一个结论，所以只此一份。
fn next_step(
    status: Option<u16>,
    errored: bool,
    matches: bool,
    result: &Result<Value, Failure>,
) -> Next {
    let fail = |failure: PoolFailure| {
        Next::Finish(ResponseHeadResult::Fail {
            reason: failure.reason(),
        })
    };
    if status.is_some_and(|status| status != 200) {
        // 错误内容与原有反馈/重试策略由内核处理，不能用插件错误覆盖上游错误。
        return Next::Finish(ResponseHeadResult::Accept);
    }
    let classification = match result {
        Ok(value) => value["classification"].as_str(),
        Err(_) => return fail(PoolFailure::control_plane()),
    };
    match classification {
        // 21：满血，继续交付；租约保持到整个客户流结束。
        Some("21") if matches => return Next::Keep { persist: false },
        // 29：降智。关闭当前租约，是否在同账号的其他网关上重发由内核按可重放与尝试上限决定。
        Some("29") if matches => return Next::Finish(ResponseHeadResult::Switch),
        _ => {}
    }
    // 没有响应（连接失败、超时、取消）：交回内核按原有错误处理，租约在请求结束时归还。
    if status.is_none() && errored {
        return Next::Keep { persist: true };
    }
    fail(if matches {
        PoolFailure::new("window_retired", "该窗口已被退役，无法判定响应")
    } else {
        PoolFailure::new(
            "gateway_mismatch",
            "上游响应的路由 Cookie 指向了别的网关，已放弃该窗口",
        )
    })
}

impl Pool {
    pub(crate) async fn response_head(
        self: &Arc<Self>,
        params: ResponseHeadParams,
    ) -> RpcResult<ResponseHeadResult> {
        self.sweep();
        self.restore_lease(&params.lease_id).await?;
        let status = params.status.unwrap_or(0);
        // 取出判定所需的事实；非 200 或又返回了 state 的访问在此固定计时终态。
        let known = self
            .leases
            .lock()
            .expect("leases lock")
            .get_mut(&params.lease_id)
            .map(|lease| {
                if let Some(timing) = lease.timing.as_mut()
                    && (status != 200 || params.state_returned)
                {
                    timing.end(if status == 200 { "rejected" } else { "failed" });
                }
                (
                    lease.account.clone(),
                    lease.model.clone(),
                    lease.gateway.clone(),
                    lease.cookie.clone(),
                    lease.timing.as_ref().map(|timing| timing.value()),
                    // 归还租约时的访问值：未固定终态的标为 `interrupted`，与 `finish_now` 一致。
                    lease
                        .timing
                        .as_ref()
                        .map(|timing| timing.closed_value("interrupted")),
                )
            });
        let Some((account, model, gateway, cookie, access, closed_access)) = known else {
            // 数据库也不存在有效租约事实时，无法判断网关是否一致。
            return Ok(ResponseHeadResult::Fail {
                reason: PoolFailure::new(
                    "lease_unknown",
                    "租约状态已丢失（插件刚重启），无法判定响应",
                )
                .reason(),
            });
        };
        let fail = |failure: PoolFailure| ResponseHeadResult::Fail {
            reason: failure.reason(),
        };
        let merged = merge_cookie(&cookie, &params.set_cookies);
        if merged.is_none() && !params.status.is_some_and(|status| status != 200) {
            self.finish_now(&params.lease_id, &account).await;
            return Ok(fail(PoolFailure::new(
                "cookie_invalid",
                "上游返回的路由 Cookie 无法合并，已放弃该窗口",
            )));
        }
        let matches =
            merged.as_deref().and_then(cookie_gateway).as_deref() == Some(gateway.as_str());
        // 与旧路径一致：只有 200 响应才带 state 事实，错误响应按未返回 state 记录。
        let reported_state = status == 200 && params.state_returned;
        // 业务结果与租约归还放在同一事务：结果让窗口退役之后、租约归还之前，
        // 并发的 `plan` 会看到“退役中、租约未还”的中间态而误报 no_candidate。
        // 归还与否由事务内的结果决定（21 保持租约），事务后按同一结论答复内核。
        let report = json!({"action": "businessResult", "accountId": account, "leaseId": params.lease_id,
            "model": model, "status": status, "stateReturned": reported_state, "paired": true,
            "gatewayMatches": matches, "durationMs": params.duration_ms, "access": access});
        let finish = json!({"action": "finish", "accountId": account, "leaseId": params.lease_id,
            "access": closed_access});
        let (reported, errored) = (params.status, params.error_kind.is_some());
        let (result, _released) = self
            .business_result_then_finish(report, finish, &params.lease_id, move |result| {
                matches!(
                    next_step(reported, errored, matches, result),
                    Next::Finish(_)
                )
            })
            .await;
        match next_step(reported, errored, matches, &result) {
            Next::Keep { persist } => {
                if persist {
                    self.persist_current(&params.lease_id).await?;
                }
                Ok(ResponseHeadResult::Accept)
            }
            Next::Finish(answer) => {
                // 忘掉内存里的租约状态；归还没成功时由租约超时回收兜底。
                self.leases
                    .lock()
                    .expect("leases lock")
                    .remove(&params.lease_id);
                Ok(answer)
            }
        }
    }

    /// 立即归还租约并忘掉它的内存状态；访问计时在未固定终态时标为 `interrupted`。
    async fn finish_now(&self, lease_id: &str, account: &str) {
        let access = self
            .leases
            .lock()
            .expect("leases lock")
            .remove(lease_id)
            .and_then(|mut lease| lease.timing.take())
            .map(|mut timing| {
                timing.end("interrupted");
                timing.value()
            });
        self.release(account, lease_id, access).await;
    }

    pub(crate) async fn finish_request(self: &Arc<Self>, params: RequestFinishParams) {
        let _ = self.restore_lease(&params.lease_id).await;
        let outcome = match params.outcome {
            RequestOutcome::Completed => "completed",
            RequestOutcome::Failed => "failed",
            RequestOutcome::Cancelled => "cancelled",
            RequestOutcome::Interrupted => "interrupted",
        };
        let lease = self
            .leases
            .lock()
            .expect("leases lock")
            .remove(&params.lease_id);
        self.requests
            .lock()
            .expect("requests lock")
            .remove(&params.request_id);
        let (account, access) = match lease {
            Some(mut lease) => {
                // 耗时从登记发送算起，到请求结束；首字时间由内核给出。
                let access = lease.timing.take().map(|mut timing| {
                    timing.end(outcome);
                    let mut value = timing.value();
                    if let Some(first_byte) = params.first_byte_ms {
                        value["firstByteMs"] = json!(first_byte);
                    }
                    value
                });
                (Some(lease.account), access)
            }
            // 插件重启或热更新后接管未结束的租约：从存储找到账号，没有访问计时可补。
            None => {
                let store = Arc::clone(&self.store);
                let id = params.lease_id.clone();
                let account = tokio::task::spawn_blocking(move || store.lease_account(&id))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
                (account, None)
            }
        };
        if let Some(account) = account {
            self.release(&account, &params.lease_id, access).await;
        }
    }
}

//! 普通账号降智确认与双向移组，沿用旧路径的确认窗口和运行状态机去重。
use super::{PROBE_ATTEMPT_MS, Pool};
use crate::caps::Caps;
use plugin_sdk::{RpcResult, protocol::*};
use serde_json::json;
use std::sync::Arc;

enum Confirmation {
    Confirmed,
    Normal,
    Unknown,
}

/// 与旧 merged_cookie_pairs 相同：只取 Set-Cookie 的 name=value 部分。
pub(super) fn cookie_pairs(updates: &[String]) -> Option<String> {
    let pairs = updates
        .iter()
        .filter_map(|update| update.split(';').next())
        .filter(|pair| pair.contains('='))
        .map(str::trim)
        .collect::<Vec<_>>();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

impl Pool {
    pub(crate) async fn observe(
        self: &Arc<Self>,
        caps: &Caps,
        params: ResponseObserveParams,
    ) -> RpcResult<ResponseObserveResult> {
        if self.is_stopped()
            || !params.sse
            || params.status != Some(200)
            || !params.request_had_turn_state
            || params.turn_state.is_none()
            || !self
                .settings()
                .await
                .is_ok_and(|s| s.dynamic_groups_ready())
        {
            return Ok(ResponseObserveResult::Accept);
        }
        if matches!(self.confirm(caps, &params).await, Confirmation::Confirmed) {
            // 与旧 report_degraded 一样，移组在后台完成，失败不影响当前响应或换号。
            self.report_regroup(caps, &params.account_id, "degraded", &params.model);
            if params.can_switch {
                return Ok(ResponseObserveResult::SwitchAccount { reason: Reason {
                    code: "account_degraded".into(),
                    message: "网关池：账号返回降智信号（带 state 的请求又返回 state，确认请求同样如此），本次请求交给其他账号重放".into(),
                }});
            }
        }
        Ok(ResponseObserveResult::Accept)
    }

    async fn confirm(&self, caps: &Caps, params: &ResponseObserveParams) -> Confirmation {
        let now = self.now();
        {
            let mut recent = self.confirmations.lock().expect("confirmations lock");
            recent.retain(|_, at| now - *at < 20.0);
            if recent.contains_key(&params.account_id) {
                return Confirmation::Unknown;
            }
            recent.insert(params.account_id.clone(), now);
        }
        let model = &params.model;
        if model.is_empty()
            || model.len() > 64
            || !model
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        {
            return Confirmation::Unknown;
        }
        if self.is_stopped() {
            return Confirmation::Unknown;
        }
        match caps
            .probe(UpstreamProbeParams {
                account_id: params.account_id.clone(),
                model: model.clone(),
                turn_state: params.turn_state.clone(),
                cookie: cookie_pairs(&params.set_cookies),
                include_account_cookies: true,
                timeout_ms: PROBE_ATTEMPT_MS,
                request_id: None,
            })
            .await
        {
            Ok(result) if result.status == Some(200) => {
                if result.state_returned {
                    Confirmation::Confirmed
                } else {
                    Confirmation::Normal
                }
            }
            _ => Confirmation::Unknown,
        }
    }

    pub(super) fn report_regroup(
        self: &Arc<Self>,
        caps: &Caps,
        account: &str,
        direction: &'static str,
        model: &str,
    ) {
        let (pool, caps, account, model) = (
            self.clone(),
            caps.clone(),
            account.to_owned(),
            model.to_owned(),
        );
        tokio::spawn(async move {
            let Ok(reply) = pool.runtime(json!({"action":"regroup", "accountId":account, "direction":direction,"model":model})).await else { return; };
            let Some(event) = reply["move"]["eventId"].as_str() else {
                return;
            };
            let status = match caps
                .move_group(GroupsMoveParams {
                    account_id: account.clone(),
                    remove_group: reply["move"]["removeGroup"].as_str().map(str::to_owned),
                    add_group: reply["move"]["addGroup"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    reason: Reason {
                        code: format!("gateway_pool:{direction}"),
                        message: String::new(),
                    },
                })
                .await
            {
                Ok(GroupsMoveResult {
                    outcome: MoveOutcome::Moved,
                }) => "moved",
                Ok(_) => "unchanged",
                Err(_) => "failed",
            };
            let _ = pool.runtime(json!({"action":"regroupResult", "accountId":account, "eventId":event, "status":status})).await;
        });
    }
}

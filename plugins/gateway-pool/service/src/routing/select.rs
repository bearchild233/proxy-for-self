//! `route.select`：等价于旧路径的 `selection` + `ordered_accounts`，并给出原生续接的去留。

use std::collections::{HashMap, HashSet};

use plugin_sdk::{RpcResult, protocol::*};
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::{Pool, PoolFailure, RequestState, SELECT_KEEP_SECONDS};
use crate::caps::{Caps, QuotaView};

fn blocked_entry(account_id: &str, failure: &PoolFailure) -> BlockedAccount {
    BlockedAccount {
        account_id: account_id.to_owned(),
        reason: failure.reason(),
    }
}

/// 原生续接固定在 `bound` 账号上时的去留：别的账号持有窗口而它没有，或它不可调度但还有退路时，
/// 交给内核按安全规则恢复历史（`Escape`）；它自己持窗，或没有任何退路时留下（`Stay`）。
/// 与旧 `provider/mod.rs` 的续接去留规则逐项相同。
pub(super) fn pinned_decision(
    bound: &str,
    preferred: &[String],
    live: &[String],
    ordinary_available: bool,
) -> PinnedDecision {
    let bound_live = live.iter().any(|id| id == bound);
    let live_elsewhere = live.iter().any(|id| id != bound);
    let bound_unusable = !preferred.iter().any(|id| id == bound);
    if (live_elsewhere && !bound_live)
        || (bound_unusable && (ordinary_available || !preferred.is_empty()))
    {
        PinnedDecision::Escape
    } else {
        PinnedDecision::Stay
    }
}

impl Pool {
    pub(crate) async fn select(
        self: &std::sync::Arc<Self>,
        caps: &Caps,
        params: RouteSelectParams,
    ) -> RpcResult<RouteSelectResult> {
        self.sweep();
        // 读不到设置、账号、额度或调度状态时整体不可用：内核据此让本插件的账号失败关闭。
        let unavailable = |what: &'static str| move |_| RpcError::new(UNAVAILABLE, what);
        let settings = self.settings().await.map_err(unavailable("settings"))?;
        let rows = caps
            .accounts(&params.accounts, &settings.probe_model)
            .await?;

        let mut blocked = Vec::new();
        let mut candidates = Vec::new();
        for id in &params.accounts {
            match rows.iter().find(|row| &row.id == id) {
                None => blocked.push(blocked_entry(
                    id,
                    &PoolFailure::new("account_missing", "账号已被删除或归档"),
                )),
                // 网关池只支持 OAuth 账号。
                Some(row) if row.authentication_kind != "oauth" => blocked.push(blocked_entry(
                    id,
                    &PoolFailure::new("not_oauth", "网关池只支持 OAuth 账号"),
                )),
                Some(row) if row.model_allowed == Some(false) => blocked.push(blocked_entry(
                    id,
                    &Self::model_denied(&settings.probe_model),
                )),
                Some(row) => candidates.push(row),
            }
        }

        // 额度耗尽：账号状态已是耗尽，或调度信号显示剩余额度排名最低且快照确认耗尽。
        let mut checks = JoinSet::new();
        for row in &candidates {
            if row.status != "quota_exhausted" {
                let (caps, id) = (caps.clone(), row.id.clone());
                checks.spawn(async move { (id.clone(), caps.quota(&id).await) });
            }
        }
        let mut quotas: HashMap<String, QuotaView> = HashMap::new();
        while let Some(joined) = checks.join_next().await {
            let (id, quota) = joined.map_err(|_| RpcError::new(INTERNAL, "quota.read"))?;
            quotas.insert(id, quota?);
        }
        candidates.retain(|row| {
            if quotas.get(&row.id).is_some_and(|quota| !quota.present) {
                blocked.push(blocked_entry(
                    &row.id,
                    &PoolFailure::new("quota_unreadable", "读取不到账号额度，网关池暂不检测该账号"),
                ));
                false
            } else {
                true
            }
        });
        let candidate_json: Vec<Value> = candidates
            .iter()
            .map(|row| {
                let exhausted = row.status == "quota_exhausted"
                    || quotas
                        .get(&row.id)
                        .is_some_and(|quota| quota.rank == Some(0) && quota.exhausted);
                json!({"id": row.id, "exhausted": exhausted})
            })
            .collect();

        let (mut preferred, mut live) = (Vec::new(), Vec::new());
        if !candidate_json.is_empty() {
            let value = self
                .runtime(json!({"action": "active", "candidates": candidate_json}))
                .await
                .map_err(unavailable("active"))?;
            let known: HashSet<&str> = params.accounts.iter().map(String::as_str).collect();
            let ids = |value: &Value| -> Vec<String> {
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|id| known.contains(id))
                    .map(str::to_owned)
                    .collect()
            };
            // 与旧路径一致：缺少 `accounts` 数组视为调度状态不可用。
            value["accounts"]
                .as_array()
                .ok_or_else(|| RpcError::new(UNAVAILABLE, "active"))?;
            preferred = ids(&value["accounts"]);
            live = ids(&value["live"]);
            for item in value["blocked"].as_array().into_iter().flatten() {
                if let (Some(id), Some(failure)) =
                    (item["accountId"].as_str(), PoolFailure::from_block(item))
                    && known.contains(id)
                {
                    blocked.push(blocked_entry(id, &failure));
                }
            }
        }

        let pinned_decision = params
            .pinned
            .as_deref()
            .map(|bound| pinned_decision(bound, &preferred, &live, params.ordinary_available));

        // 记下候选，供后续 `route.prepare` 的 plan/claim 判断账号能否让位。
        let expires = self.now() + SELECT_KEEP_SECONDS;
        let mut requests = self.requests.lock().expect("requests lock");
        let state = requests
            .entry(params.request_id.clone())
            .or_insert_with(|| RequestState {
                expires,
                alternatives: Vec::new(),
                ordinary: false,
                attempts: HashMap::new(),
            });
        state.expires = state.expires.max(expires);
        state.alternatives = preferred.clone();
        state.ordinary = params.ordinary_available;
        drop(requests);

        Ok(RouteSelectResult {
            preferred,
            live,
            blocked,
            pinned_decision,
        })
    }
}

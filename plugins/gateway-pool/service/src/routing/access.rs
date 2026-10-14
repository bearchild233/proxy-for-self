//! 账号资格与网关目录：对应旧路径的 `eligible_account`、`ineligible` 与 `catalog`。

use std::{sync::Arc, time::Instant};

use plugin_sdk::protocol::AccountSummary;
use serde_json::{Value, json};

use super::{CATALOG_KEEP, CATALOG_TTL, HOOK_BUDGET, Pool, PoolFailure, PoolSettings};
use crate::{caps::Caps, pool_service};

/// 网关目录读取的结果。
pub(super) enum CatalogState {
    /// 目录（读取失败时为 `Null`，由运行状态机退回限定列表或上一份目录）。
    Ready(Value),
    /// 读取仍在进行：本次钩子的预算用完了。
    Pending,
}

impl Pool {
    /// 读取单个账号；账号不在本插件的分组里时内核不返回，按已删除或归档处理。
    pub(super) async fn load_account(
        &self,
        caps: &Caps,
        id: &str,
    ) -> Result<Option<AccountSummary>, PoolFailure> {
        let settings = self.settings().await?;
        caps.accounts(&[id.to_owned()], &settings.probe_model)
            .await
            .map(|rows| rows.into_iter().find(|row| row.id == id))
            .map_err(|_| PoolFailure::kernel())
    }

    /// 账号能否参与网关池：启用、OAuth、状态正常、额度未耗尽。不满足时返回具体原因。
    /// 同时把额度耗尽与否同步给运行状态机（与旧 `eligible_account` 相同）。
    ///
    /// 协议 0.6 提供打票模型权限与缺失额度刷新，恢复旧路径的两项准入检查。
    pub(super) async fn eligible(
        &self,
        caps: &Caps,
        row: &AccountSummary,
    ) -> Result<(), PoolFailure> {
        if !row.enabled {
            return Err(PoolFailure::new("account_disabled", "账号已停用"));
        }
        if row.authentication_kind != "oauth" {
            return Err(PoolFailure::new("not_oauth", "网关池只支持 OAuth 账号"));
        }
        if row.model_allowed == Some(false) {
            let settings = self.settings().await?;
            return Err(Self::model_denied(&settings.probe_model));
        }
        match row.status.as_str() {
            "normal" => {}
            "quota_exhausted" => {
                self.set_exhausted(&row.id, true).await?;
                return Err(PoolFailure::new(
                    "quota_exhausted",
                    "账号额度已耗尽，等待额度重置",
                ));
            }
            "rate_limited" => {
                return Err(PoolFailure::new(
                    "rate_limited",
                    "账号正在限流冷却，稍后再试",
                ));
            }
            "disabled" => return Err(PoolFailure::new("account_disabled", "账号已停用")),
            _ => {
                return Err(PoolFailure::new(
                    "account_error",
                    "账号处于错误状态，需要重新授权或检查凭据",
                ));
            }
        }
        let quota = caps
            .quota(&row.id)
            .await
            .map_err(|_| PoolFailure::kernel())?;
        if !quota.present {
            return Err(PoolFailure::new(
                "quota_unreadable",
                "读取不到账号额度，网关池暂不检测该账号",
            ));
        }
        self.set_exhausted(&row.id, quota.exhausted).await?;
        if quota.exhausted {
            return Err(PoolFailure::new(
                "quota_exhausted",
                "账号额度已耗尽，等待额度重置",
            ));
        }
        Ok(())
    }

    async fn set_exhausted(&self, account: &str, exhausted: bool) -> Result<(), PoolFailure> {
        self.runtime(json!({"action": "quota", "accountId": account, "exhausted": exhausted}))
            .await?;
        Ok(())
    }

    pub(super) fn model_denied(model: &str) -> PoolFailure {
        PoolFailure::new(
            "no_probe_model_access",
            format!("账号没有打票模型 {model} 的使用权限，无法检测网关"),
        )
    }

    fn cached_catalog(&self, identity: &str) -> Option<Value> {
        self.catalogs
            .lock()
            .expect("catalogs lock")
            .get(identity)
            .filter(|(at, _)| at.elapsed() < CATALOG_TTL)
            .map(|(_, value)| value.clone())
    }

    /// 读取并缓存目录；失败返回 `Null`。
    async fn fetch_catalog(
        &self,
        caps: &Caps,
        settings: &PoolSettings,
        account: &str,
        identity: &str,
    ) -> Value {
        let Some(value) = pool_service::catalog(caps, settings, account, identity).await else {
            return Value::Null;
        };
        let mut catalogs = self.catalogs.lock().expect("catalogs lock");
        catalogs.retain(|_, (at, _)| at.elapsed() < CATALOG_KEEP);
        catalogs.insert(identity.to_owned(), (Instant::now(), value.clone()));
        value
    }

    /// 后台任务用：命中缓存直接返回，否则读取到完成。
    pub(super) async fn catalog_value(
        &self,
        caps: &Caps,
        settings: &PoolSettings,
        account: &str,
        identity: &str,
    ) -> Value {
        if let Some(value) = self.cached_catalog(identity) {
            return value;
        }
        self.fetch_catalog(caps, settings, account, identity).await
    }

    /// 钩子用：命中缓存直接返回；否则读取，等待不超过钩子预算，超出时读取在后台继续、本次返回 `Pending`。
    /// 同一成员已有读取在进行时也返回 `Pending`，等它写入缓存。
    pub(super) async fn catalog_within(
        self: &Arc<Self>,
        caps: &Caps,
        settings: &Arc<PoolSettings>,
        account: &str,
        identity: &str,
        started: Instant,
    ) -> CatalogState {
        if let Some(value) = self.cached_catalog(identity) {
            return CatalogState::Ready(value);
        }
        if !self
            .catalog_inflight
            .lock()
            .expect("catalog inflight lock")
            .insert(identity.to_owned())
        {
            return CatalogState::Pending;
        }
        let (pool, caps, settings) = (Arc::clone(self), caps.clone(), Arc::clone(settings));
        let (account, identity) = (account.to_owned(), identity.to_owned());
        let mut task = tokio::spawn(async move {
            let value = pool
                .fetch_catalog(&caps, &settings, &account, &identity)
                .await;
            pool.catalog_inflight
                .lock()
                .expect("catalog inflight lock")
                .remove(&identity);
            value
        });
        match tokio::time::timeout(HOOK_BUDGET.saturating_sub(started.elapsed()), &mut task).await {
            Ok(Ok(value)) => CatalogState::Ready(value),
            Ok(Err(_)) => CatalogState::Ready(Value::Null),
            Err(_) => CatalogState::Pending,
        }
    }
}

//! 插件账号目录复用管理投影，不暴露 credential 文档。
use async_trait::async_trait;
use gateway_admin::ports::store::AdminStorePorts;
use gateway_core::{
    account::{ProviderAccountId, ProviderAccountStore},
    plugin::{AccountSummary, PluginAccounts, RpcError, UNAVAILABLE},
};
use std::sync::Arc;

pub(crate) struct PluginAccountDirectory {
    pub accounts: Arc<dyn ProviderAccountStore>,
    pub admin: AdminStorePorts,
}
#[async_trait]
impl PluginAccounts for PluginAccountDirectory {
    async fn read(
        &self,
        ids: Option<&[String]>,
        model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError> {
        let unavailable = |_| RpcError::new(UNAVAILABLE, "account directory");
        let ids = match ids {
            Some(ids) => ids.to_vec(),
            None => self
                .accounts
                .list_accounts()
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account directory"))?
                .into_iter()
                .map(|account| account.id().as_str().to_owned())
                .collect(),
        };
        let runtime = self
            .admin
            .account_runtime()
            .account_runtime(&ids)
            .await
            .map_err(unavailable)?;
        let mut rows = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(item) = self
                .admin
                .accounts()
                .load_account(&id, runtime.clone())
                .await
                .map_err(unavailable)?
            {
                let account = item.account;
                // 模型权限复用账号领域值，不在目录投影中重新解释策略。
                let model_allowed = if let Some(model) = model {
                    let id = ProviderAccountId::new(&id)
                        .map_err(|_| RpcError::new(UNAVAILABLE, "account directory"))?;
                    let Some(current) = self
                        .accounts
                        .get_account(&id)
                        .await
                        .map_err(|_| RpcError::new(UNAVAILABLE, "account directory"))?
                    else {
                        continue;
                    };
                    Some(current.model_access().allows(model))
                } else {
                    None
                };
                rows.push(AccountSummary {
                    model_allowed,
                    id: account.id,
                    enabled: account.enabled,
                    status: item.projection.status.as_str().into(),
                    authentication_kind: account.authentication_kind,
                    groups: account
                        .groups
                        .into_iter()
                        .map(|group| group.id.as_str().to_owned())
                        .collect(),
                    upstream_account_id: account.upstream_account_id,
                    user_id: account.upstream_user_id,
                });
            }
        }
        Ok(rows)
    }
}

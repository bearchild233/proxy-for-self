use super::*;
use crate::{
    account::ProviderAccountId,
    provider_ports::{AccountGroupMove, AccountGroupMoveOutcome, ProviderGroupMovePort},
};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::sync::Arc;

pub struct KernelCapabilities {
    pub accounts: Arc<dyn PluginAccounts>,
    pub upstream: Arc<dyn PluginUpstream>,
    pub groups: Arc<dyn ProviderGroupMovePort>,
    pub log: Arc<dyn PluginCapabilities>,
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|_| RpcError::new(INVALID_PARAMS, "invalid capability parameters"))
}
fn encode(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|_| RpcError::new(INTERNAL, "capability result"))
}
pub fn validate_timeout(timeout_ms: u64) -> Result<(), RpcError> {
    if !(1000..=30000).contains(&timeout_ms) {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "timeoutMs must be 1000..30000",
        ));
    }
    Ok(())
}
/// 账号分组由存储现读，插件分组随后读取，避免沿用握手时的授权快照。
pub async fn require_account(
    access: &dyn PluginAccess,
    accounts: &dyn PluginAccounts,
    id: &str,
) -> Result<(), RpcError> {
    ProviderAccountId::new(id).map_err(|_| RpcError::new(INVALID_PARAMS, "accountId"))?;
    let rows = accounts.read(Some(&[id.to_owned()]), None).await?;
    if !rows
        .iter()
        .any(|row| row.id == id && access.allows(&row.groups))
    {
        return Err(RpcError::new(FORBIDDEN, "account outside plugin groups"));
    }
    Ok(())
}
#[async_trait]
impl PluginCapabilities for KernelCapabilities {
    async fn call(
        &self,
        access: &dyn PluginAccess,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        match method {
            "log" => self.log.call(access, method, params).await,
            "accounts.read" => {
                let params: AccountsReadParams = decode(params)?;
                for id in params.ids.iter().flatten() {
                    ProviderAccountId::new(id).map_err(|_| RpcError::new(INVALID_PARAMS, "ids"))?;
                }
                let rows = self
                    .accounts
                    .read(params.ids.as_deref(), params.model.as_deref())
                    .await?;
                encode(AccountsReadResult {
                    accounts: rows
                        .into_iter()
                        .filter(|row| access.allows(&row.groups))
                        .collect(),
                })
            }
            "quota.read" => {
                let params: QuotaReadParams = decode(params)?;
                require_account(access, self.accounts.as_ref(), &params.account_id).await?;
                encode(self.upstream.quota(params).await?)
            }
            "groups.move" => {
                let params: GroupsMoveParams = decode(params)?;
                crate::account::scope::AccountGroupId::new(&params.add_group)
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "addGroup"))?;
                if let Some(group) = &params.remove_group {
                    crate::account::scope::AccountGroupId::new(group)
                        .map_err(|_| RpcError::new(INVALID_PARAMS, "removeGroup"))?;
                    if group == &params.add_group {
                        return Err(RpcError::new(
                            INVALID_PARAMS,
                            "source and target groups must differ",
                        ));
                    }
                }
                require_account(access, self.accounts.as_ref(), &params.account_id).await?;
                let groups = access.groups();
                if !groups.contains(&params.add_group)
                    || params
                        .remove_group
                        .as_ref()
                        .is_some_and(|group| !groups.contains(group))
                {
                    return Err(RpcError::new(FORBIDDEN, "group outside plugin groups"));
                }
                let outcome = self
                    .groups
                    .move_account_group(&AccountGroupMove {
                        account: ProviderAccountId::new(params.account_id)
                            .map_err(|_| RpcError::new(INVALID_PARAMS, "accountId"))?,
                        remove_group: params.remove_group,
                        add_group: params.add_group,
                        reason: format!("plugin:{}:{}", access.id(), params.reason.code),
                    })
                    .await
                    .map_err(|_| RpcError::new(UNAVAILABLE, "groups.move"))?;
                encode(GroupsMoveResult {
                    outcome: match outcome {
                        AccountGroupMoveOutcome::Moved => MoveOutcome::Moved,
                        AccountGroupMoveOutcome::Unchanged => MoveOutcome::Unchanged,
                    },
                })
            }
            "upstream.probe" => {
                let params: UpstreamProbeParams = decode(params)?;
                crate::routing::UpstreamModelId::new(&params.model)
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "model"))?;
                validate_timeout(params.timeout_ms)?;
                require_account(access, self.accounts.as_ref(), &params.account_id).await?;
                encode(
                    self.upstream
                        .probe(access, self.accounts.as_ref(), params)
                        .await?,
                )
            }
            "http.fetch" => {
                let params: HttpFetchParams = decode(params)?;
                validate_timeout(params.timeout_ms)?;
                if params
                    .body
                    .as_ref()
                    .is_some_and(|body| body.len() > 512 * 1024)
                {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "request body exceeds 512 KiB",
                    ));
                }
                let url = url::Url::parse(&params.url)
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "url"))?;
                if url.scheme() != "https" {
                    return Err(RpcError::new(INVALID_PARAMS, "https required"));
                }
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(RpcError::new(
                        INVALID_PARAMS,
                        "URL credentials are not allowed",
                    ));
                }
                let host = url
                    .host_str()
                    .ok_or_else(|| RpcError::new(INVALID_PARAMS, "host"))?;
                if !access.fetch_hosts().iter().any(|allowed| {
                    allowed == &format!("{host}:{}", url.port_or_known_default().unwrap_or(443))
                        || allowed == host && url.port_or_known_default() == Some(443)
                }) {
                    return Err(RpcError::new(FORBIDDEN, "host outside fetchHosts"));
                }
                if let Egress::Account { account_id } = &params.egress {
                    require_account(access, self.accounts.as_ref(), account_id).await?;
                }
                encode(self.upstream.fetch(params).await?)
            }
            _ => Err(RpcError::new(NOT_SUPPORTED, method)),
        }
    }
}

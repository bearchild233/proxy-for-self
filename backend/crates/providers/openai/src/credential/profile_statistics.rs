//! OpenAI 当前账号个人资料统计与官方头像查询编排。

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use gateway_core::account::ProviderAccountId;
use reqwest::Client;
use secrecy::{ExposeSecret as _, SecretString};
use thiserror::Error;
use uuid::Uuid;

use crate::transport::profile::CodexWireProfileState;
use crate::transport::subscription::CodexSubscription;
use crate::transport::{
    CodexBackendClient, CodexClientError, CodexProfileAvatar, CodexProfileAvatarFetchError,
    CodexProfileStatistics, CodexRequestContext, fetch_profile_avatar,
};

use super::CodexCredentialRepository;

const PROFILE_AVATAR_SOURCE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Error)]
pub enum CodexProfileStatisticsError {
    #[error("Codex profile-statistics credential data is invalid")]
    InvalidCredentialData,
    #[error("Codex OAuth access token must be refreshed before querying profile statistics")]
    CredentialRefreshRequired { upstream_body: Option<String> },
    #[error("Codex profile-statistics account was not found")]
    NotFound,
    #[error("provider account store is unavailable: {detail}")]
    Store { detail: String },
    #[error("Codex profile-statistics upstream returned HTTP {status}")]
    Upstream {
        status: u16,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("Codex profile-statistics query transport is unavailable")]
    TransportUnavailable,
}

impl std::fmt::Debug for CodexProfileStatisticsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCredentialData => formatter.write_str("InvalidCredentialData"),
            Self::CredentialRefreshRequired { .. } => {
                formatter.write_str("CredentialRefreshRequired { upstream_body: <redacted> }")
            }
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Store { .. } => formatter.write_str("Store { detail: <redacted> }"),
            Self::Upstream {
                status,
                retry_after_seconds,
                ..
            } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .field("body", &"<redacted>")
                .field("retry_after_seconds", retry_after_seconds)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
        }
    }
}

#[derive(Error)]
pub enum CodexProfileAvatarError {
    #[error(transparent)]
    ProfileStatistics(#[from] CodexProfileStatisticsError),
    #[error("Codex profile does not provide an avatar")]
    Missing,
    #[error("Codex profile avatar source is invalid")]
    InvalidSource,
    #[error("Codex profile avatar upstream returned HTTP {status}")]
    Upstream { status: u16 },
    #[error("Codex profile avatar transport is unavailable")]
    TransportUnavailable,
}

impl std::fmt::Debug for CodexProfileAvatarError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProfileStatistics(error) => formatter
                .debug_tuple("ProfileStatistics")
                .field(error)
                .finish(),
            Self::Missing => formatter.write_str("Missing"),
            Self::InvalidSource => formatter.write_str("InvalidSource"),
            Self::Upstream { status } => formatter
                .debug_struct("Upstream")
                .field("status", status)
                .finish(),
            Self::TransportUnavailable => formatter.write_str("TransportUnavailable"),
        }
    }
}

#[derive(Clone)]
struct CachedProfileAvatarSource {
    source: String,
    expires_at: Instant,
}

pub struct CodexCredentialProfileService {
    refresher: Option<std::sync::Arc<super::CodexCredentialRefreshService>>,
    repository: CodexCredentialRepository,
    profile: CodexWireProfileState,
    http: Client,
    base_url: String,
    lifecycle_attempts: Mutex<HashMap<ProviderAccountId, Instant>>,
    avatar_sources: Mutex<HashMap<ProviderAccountId, CachedProfileAvatarSource>>,
    subscription_queue:
        super::refresh_queue::RefreshQueue<Option<CodexSubscription>, CodexProfileStatisticsError>,
    statistics_queue:
        super::refresh_queue::RefreshQueue<CodexProfileStatistics, CodexProfileStatisticsError>,
}

impl CodexCredentialProfileService {
    #[must_use]
    pub fn new(
        repository: CodexCredentialRepository,
        profile: CodexWireProfileState,
        http: Client,
        base_url: String,
    ) -> Self {
        Self {
            refresher: None,
            repository,
            profile,
            http,
            base_url,
            lifecycle_attempts: Mutex::new(HashMap::new()),
            avatar_sources: Mutex::new(HashMap::new()),
            subscription_queue: Default::default(),
            statistics_queue: Default::default(),
        }
    }

    #[must_use]
    pub fn with_refresher(
        mut self,
        refresher: std::sync::Arc<super::CodexCredentialRefreshService>,
    ) -> Self {
        self.refresher = Some(refresher);
        self
    }

    /// 独立维护订阅事实，所有查询复用账号自己的出口；临时错误不会归档。
    pub async fn maintain_lifecycle(&self) -> Result<(), CodexProfileStatisticsError> {
        use gateway_core::account::CredentialState;
        let accounts = self
            .repository
            .store()
            .list_accounts()
            .await
            .map_err(|error| CodexProfileStatisticsError::Store {
                detail: error.to_string(),
            })?;
        let now = chrono::Utc::now();
        for mut account in accounts {
            if account.provider().as_str() != "openai"
                || account.authentication_kind() != "oauth"
                || account.lifecycle().archived
                || (!account.enabled() && account.lifecycle().archived_at.is_some())
            {
                continue;
            }
            let mut lifecycle = account.lifecycle().clone();
            if matches!(
                account.credential_state(),
                CredentialState::Invalid | CredentialState::Banned
            ) || (account.credential_state() == CredentialState::Expired
                && account.last_error_reason()
                    == Some(gateway_core::account::AccountErrorReason::CredentialExpired))
            {
                lifecycle.archived = true;
                lifecycle.archive_reason = Some("credential_invalid".to_owned());
                lifecycle.archived_at = Some(now);
            } else {
                if !account.enabled() {
                    continue;
                }
                let should_probe = {
                    let mut attempts = self
                        .lifecycle_attempts
                        .lock()
                        .expect("lifecycle attempts mutex");
                    attempts.retain(|_, at| at.elapsed() < Duration::from_secs(3600));
                    if attempts.contains_key(account.id()) {
                        false
                    } else {
                        attempts.insert(account.id().clone(), Instant::now());
                        true
                    }
                };
                if !should_probe {
                    continue;
                }
                let mut result = self.subscription(account.id()).await;
                if matches!(
                    result,
                    Err(CodexProfileStatisticsError::CredentialRefreshRequired { .. })
                ) {
                    let Some(refresher) = &self.refresher else {
                        continue;
                    };
                    let outcome = refresher.refresh_rejected_account(&account).await;
                    if !matches!(
                        outcome,
                        Ok(super::CodexCredentialRefreshOutcome::Refreshed { .. }
                            | super::CodexCredentialRefreshOutcome::Invalidated { .. }
                            | super::CodexCredentialRefreshOutcome::Banned { .. })
                    ) {
                        continue;
                    }
                    let Some(current) = self
                        .repository
                        .store()
                        .get_account(account.id())
                        .await
                        .map_err(|error| CodexProfileStatisticsError::Store {
                            detail: error.to_string(),
                        })?
                    else {
                        continue;
                    };
                    // 刷新期间可能重新授权或人工恢复；只归档仍然确认失效的当前凭据。
                    if current.lifecycle().archived
                        || (!current.enabled() && current.lifecycle().archived_at.is_some())
                        || current.upstream_account_id() != account.upstream_account_id()
                    {
                        continue;
                    }
                    account = current;
                    lifecycle = account.lifecycle().clone();
                    if matches!(
                        outcome,
                        Ok(super::CodexCredentialRefreshOutcome::Invalidated { .. }
                            | super::CodexCredentialRefreshOutcome::Banned { .. })
                    ) {
                        if !matches!(
                            account.credential_state(),
                            CredentialState::Invalid | CredentialState::Banned
                        ) && !(account.credential_state() == CredentialState::Expired
                            && account.last_error_reason()
                                == Some(
                                    gateway_core::account::AccountErrorReason::CredentialExpired,
                                ))
                        {
                            continue;
                        }
                        lifecycle.archived = true;
                        lifecycle.archive_reason = Some("credential_invalid".to_owned());
                        lifecycle.archived_at = Some(now);
                        self.repository
                            .store()
                            .compare_and_swap_lifecycle(&account, lifecycle)
                            .await
                            .map_err(|error| CodexProfileStatisticsError::Store {
                                detail: error.to_string(),
                            })?;
                        continue;
                    }
                    result = self.subscription(account.id()).await;
                }
                let Ok(Some(subscription)) = result else {
                    continue;
                };
                lifecycle.subscription_expires_at = Some(subscription.expires_at);
                lifecycle.subscription_observed_at = Some(subscription.observed_at);
                // 自动续费周期切换时不以旧账期判死；只有确认不续费的到期订阅自动归档。
                if subscription.expires_at <= now && subscription.will_renew == Some(false) {
                    lifecycle.archived = true;
                    lifecycle.archive_reason = Some("subscription_expired".to_owned());
                    lifecycle.archived_at = Some(now);
                }
            }
            self.repository
                .store()
                .compare_and_swap_lifecycle(&account, lifecycle)
                .await
                .map_err(|error| CodexProfileStatisticsError::Store {
                    detail: error.to_string(),
                })?;
        }
        Ok(())
    }

    /// 订阅只按需读取；多设备共享短期结果，不刷新凭据或更新额度状态。
    pub async fn subscription(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<Option<CodexSubscription>, CodexProfileStatisticsError> {
        let (authorization, upstream_account_id, account) =
            self.account_authentication(account_id).await?;
        let Some(upstream_account_id) = upstream_account_id else {
            return Ok(None);
        };
        let request_id = format!("subscription_{}", Uuid::now_v7().simple());
        let subscription = self
            .subscription_queue
            .run(
                format!("{}:{:?}", account_id, account.revision()),
                false,
                Duration::from_secs(15),
                CodexProfileStatisticsError::TransportUnavailable,
                async {
                    let subscription = CodexBackendClient::new(
                        self.http.clone(),
                        self.base_url.clone(),
                        self.profile.clone(),
                    )
                    .for_account(&account)
                    .map_err(map_client_error)?
                    .fetch_subscription_checked(
                        CodexRequestContext::auxiliary(
                            authorization.expose_secret(),
                            Some(&upstream_account_id),
                            &request_id,
                            None,
                        ),
                        &upstream_account_id,
                    )
                    .await
                    .map_err(|_| {
                        CodexProfileStatisticsError::CredentialRefreshRequired {
                            upstream_body: None,
                        }
                    })?;
                    if subscription.is_some() {
                        return Ok(subscription);
                    }
                    // 订阅接口可能被防护页拦截。实时额度仍能确认认证状态及已降级的套餐。
                    let client = CodexBackendClient::new(
                        self.http.clone(),
                        self.base_url.clone(),
                        self.profile.clone(),
                    )
                    .for_account(&account)
                    .map_err(map_client_error)?;
                    let usage = tokio::time::timeout(
                        Duration::from_secs(5),
                        client.fetch_usage(CodexRequestContext::auxiliary(
                            authorization.expose_secret(),
                            Some(&upstream_account_id),
                            &request_id,
                            None,
                        )),
                    )
                    .await
                    .map_err(|_| CodexProfileStatisticsError::TransportUnavailable)?
                    .map_err(map_client_error)?;
                    if usage.get("plan_type").and_then(serde_json::Value::as_str) != Some("free") {
                        return Ok(None);
                    }
                    let runtime = self
                        .repository
                        .load_runtime_credential(&account)
                        .await
                        .map_err(|_| CodexProfileStatisticsError::InvalidCredentialData)?;
                    let expiry = runtime
                        .authentication
                        .oauth()
                        .and_then(|secret| secret.id_token.as_ref())
                        .and_then(|token| {
                            super::types::subscription_expiration(
                                token.expose_secret(),
                                &upstream_account_id,
                            )
                        })
                        .filter(|expiry| *expiry <= chrono::Utc::now());
                    // 免费账号本身不会被归档；必须同时有已结束的付费订阅记录。
                    Ok(expiry.map(|expires_at| CodexSubscription {
                        starts_at: None,
                        expires_at,
                        will_renew: Some(false),
                        billing_period: None,
                        billing_currency: None,
                        observed_at: chrono::Utc::now(),
                    }))
                },
            )
            .await?;
        // 请求期间重新授权、换绑或删除账号时，丢弃旧身份的结果。
        let current = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|error| CodexProfileStatisticsError::Store {
                detail: error.to_string(),
            })?;
        if !current.is_some_and(|current| {
            current.provider() == account.provider()
                && current.revision() == account.revision()
                && current.upstream_account_id() == Some(upstream_account_id.as_str())
        }) {
            return Ok(None);
        }
        Ok(subscription)
    }

    pub async fn profile_statistics(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexProfileStatistics, CodexProfileStatisticsError> {
        let (authorization, upstream_account_id, account) =
            self.account_authentication(account_id).await?;
        let request_id = format!("profile_statistics_{}", Uuid::now_v7().simple());
        let statistics = self
            .statistics_queue
            .run(
                format!("{}:{:?}", account_id, account.revision()),
                false,
                Duration::from_secs(15),
                CodexProfileStatisticsError::TransportUnavailable,
                async {
                    CodexBackendClient::new(
                        self.http.clone(),
                        self.base_url.clone(),
                        self.profile.clone(),
                    )
                    .for_account(&account)
                    .map_err(map_client_error)?
                    .fetch_profile_statistics(CodexRequestContext::auxiliary(
                        authorization.expose_secret(),
                        upstream_account_id.as_deref(),
                        &request_id,
                        None,
                    ))
                    .await
                    .map_err(map_client_error)
                },
            )
            .await?;
        self.remember_avatar_source(account_id, statistics.image_url.as_deref());
        Ok(statistics)
    }

    /// URL 缓存不缓存认证；每次下载都重新读取所属账号当前的 token。
    async fn account_authentication(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<
        (
            SecretString,
            Option<String>,
            gateway_core::account::ProviderAccount,
        ),
        CodexProfileStatisticsError,
    > {
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|error| CodexProfileStatisticsError::Store {
                detail: error.to_string(),
            })?
            .filter(|account| account.provider().as_str() == "openai")
            .ok_or(CodexProfileStatisticsError::NotFound)?;
        if account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at <= SystemTime::now())
        {
            return Err(CodexProfileStatisticsError::CredentialRefreshRequired {
                upstream_body: None,
            });
        }
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| CodexProfileStatisticsError::InvalidCredentialData)?;
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| CodexProfileStatisticsError::InvalidCredentialData)?;
        Ok((
            authorization,
            account.upstream_account_id().map(str::to_owned),
            account,
        ))
    }

    pub async fn profile_avatar(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CodexProfileAvatar, CodexProfileAvatarError> {
        let source = match self.cached_avatar_source(account_id) {
            Some(source) => source,
            None => self
                .profile_statistics(account_id)
                .await?
                .image_url
                .ok_or(CodexProfileAvatarError::Missing)?,
        };
        let (authorization, upstream_account_id, account) =
            self.account_authentication(account_id).await?;
        let http = if account.outbound_proxy().is_some() {
            crate::transport::client::build_account_http_client(
                account.id().as_str(),
                account.outbound_proxy(),
            )
            .map_err(|_| CodexProfileAvatarError::TransportUnavailable)?
        } else {
            self.http.clone()
        };
        fetch_profile_avatar(
            &http,
            &self.base_url,
            &self.profile.snapshot(),
            &source,
            CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                upstream_account_id.as_deref(),
                "profile_avatar",
                None,
            ),
        )
        .await
        .map_err(map_avatar_fetch_error)
    }

    fn cached_avatar_source(&self, account_id: &ProviderAccountId) -> Option<String> {
        let now = Instant::now();
        let mut sources = self
            .avatar_sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sources.retain(|_, source| source.expires_at > now);
        sources.get(account_id).map(|source| source.source.clone())
    }

    fn remember_avatar_source(&self, account_id: &ProviderAccountId, source: Option<&str>) {
        let now = Instant::now();
        let mut sources = self
            .avatar_sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sources.retain(|_, source| source.expires_at > now);
        match source {
            Some(source) => {
                sources.insert(
                    account_id.clone(),
                    CachedProfileAvatarSource {
                        source: source.to_owned(),
                        expires_at: now + PROFILE_AVATAR_SOURCE_TTL,
                    },
                );
            }
            None => {
                sources.remove(account_id);
            }
        }
    }
}

fn map_avatar_fetch_error(error: CodexProfileAvatarFetchError) -> CodexProfileAvatarError {
    match error {
        CodexProfileAvatarFetchError::InvalidSource => CodexProfileAvatarError::InvalidSource,
        CodexProfileAvatarFetchError::Upstream { status } => {
            CodexProfileAvatarError::Upstream { status }
        }
        CodexProfileAvatarFetchError::TransportUnavailable => {
            CodexProfileAvatarError::TransportUnavailable
        }
    }
}

fn map_client_error(error: CodexClientError) -> CodexProfileStatisticsError {
    match error {
        CodexClientError::Upstream { status, body, .. }
            if status == reqwest::StatusCode::UNAUTHORIZED =>
        {
            CodexProfileStatisticsError::CredentialRefreshRequired {
                upstream_body: Some(body),
            }
        }
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            ..
        } => CodexProfileStatisticsError::Upstream {
            status: status.as_u16(),
            body,
            retry_after_seconds,
        },
        _ => CodexProfileStatisticsError::TransportUnavailable,
    }
}
